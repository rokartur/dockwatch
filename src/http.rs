use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct Error {
    pub message: String,
    pub status: u16,
}

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Error { message: message.into(), status: 0 }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<String> for Error {
    fn from(message: String) -> Self {
        Error::new(message)
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => Error::new("Docker did not answer in time"),
            _ => Error::new(format!("Docker connection failed: {error}")),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Docker's chunks are a few KiB; a larger size line is corrupt and would allocate it whole.
const MAX_CHUNK: usize = 64 * 1024 * 1024;

pub struct Head {
    pub status: u16,
    pub content_type: String,
    chunked: bool,
    length: Option<usize>,
}

/// One HTTP/1.1 exchange over a Unix socket. Requests send `Connection: close`, so EOF ends a body
/// that has neither chunked encoding nor a length.
pub struct Connection {
    reader: BufReader<UnixStream>,
}

impl Connection {
    /// `timeout` bounds each read and write; None waits forever, for streams.
    pub fn open(socket: &str, timeout: Option<Duration>) -> Result<Self> {
        let stream = UnixStream::connect(socket).map_err(|error| Error::new(format!("Cannot connect to {socket}: {error}")))?;
        stream.set_read_timeout(timeout)?;
        stream.set_write_timeout(timeout)?;
        Ok(Connection { reader: BufReader::with_capacity(64 * 1024, stream) })
    }

    /// A second handle on the socket; `shutdown` on it wakes a read blocked on another thread.
    pub fn handle(&self) -> Result<UnixStream> {
        Ok(self.reader.get_ref().try_clone()?)
    }

    pub fn send(&mut self, method: &str, path: &str, body: Option<(&str, &[u8])>) -> Result<()> {
        let mut head = format!("{method} {path} HTTP/1.1\r\nHost: docker\r\nConnection: close\r\n");
        match body {
            Some((content_type, bytes)) => head += &format!("Content-Type: {content_type}\r\nContent-Length: {}\r\n", bytes.len()),
            None if method != "GET" => head += "Content-Length: 0\r\n",
            None => {}
        }
        head += "\r\n";
        let stream = self.reader.get_mut();
        stream.write_all(head.as_bytes())?;
        if let Some((_, bytes)) = body {
            stream.write_all(bytes)?;
        }
        Ok(())
    }

    pub fn read_head(&mut self) -> Result<Head> {
        let status_line = self.read_line()?;
        let status = status_line.split(' ').nth(1).and_then(|code| code.parse().ok());
        let Some(status) = status else {
            return Err(Error::new(format!("Bad HTTP status line: {status_line}")));
        };
        let mut head = Head { status, content_type: String::new(), chunked: false, length: None };
        loop {
            let line = self.read_line()?;
            if line.is_empty() {
                return Ok(head);
            }
            let Some((name, value)) = line.split_once(':') else { continue };
            let value = value.trim();
            match name.to_ascii_lowercase().as_str() {
                "content-type" => head.content_type = value.to_string(),
                "content-length" => head.length = value.parse().ok(),
                "transfer-encoding" => head.chunked = value.eq_ignore_ascii_case("chunked"),
                _ => {}
            }
        }
    }

    /// Hands the body to `on_chunk` as it arrives: per HTTP chunk, or per read without chunking.
    pub fn read_body(&mut self, head: &Head, mut on_chunk: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
        if head.chunked {
            loop {
                let size_line = self.read_line()?;
                let size_text = size_line.split(';').next().unwrap_or_default().trim();
                let Ok(size) = usize::from_str_radix(size_text, 16) else {
                    return Err(Error::new(format!("Bad chunk size: {size_line}")));
                };
                if size == 0 {
                    return Ok(());
                }
                if size > MAX_CHUNK {
                    return Err(Error::new(format!("Bad chunk size: {size_line}")));
                }
                let mut chunk = vec![0; size];
                self.reader.read_exact(&mut chunk).map_err(closed)?;
                on_chunk(&chunk)?;
                self.read_line()?;
            }
        }
        let mut left = head.length.unwrap_or(usize::MAX);
        while left > 0 {
            let buffer = self.reader.fill_buf()?;
            if buffer.is_empty() {
                return match head.length {
                    Some(_) => Err(Error::new("Docker closed the connection")),
                    None => Ok(()),
                };
            }
            let count = buffer.len().min(left);
            on_chunk(&buffer[..count])?;
            self.reader.consume(count);
            left -= count;
        }
        Ok(())
    }

    fn read_line(&mut self) -> Result<String> {
        let mut line = Vec::new();
        if self.reader.read_until(b'\n', &mut line)? == 0 {
            return Err(Error::new("Docker closed the connection"));
        }
        while matches!(line.last(), Some(b'\n' | b'\r')) {
            line.pop();
        }
        Ok(String::from_utf8_lossy(&line).into_owned())
    }
}

fn closed(error: io::Error) -> Error {
    if error.kind() == io::ErrorKind::UnexpectedEof { Error::new("Docker closed the connection") } else { error.into() }
}

/// A long-lived response, process or poll on its own thread; dropping it closes the socket or kills
/// the process, which ends the thread.
pub struct Stream(Arc<Mutex<StreamSocket>>);

#[derive(Default)]
struct StreamSocket {
    closed: bool,
    socket: Option<UnixStream>,
    child: Option<Child>,
}

impl Stream {
    /// Connects on a new thread, so a daemon that stops accepting never blocks the caller.
    pub fn spawn(socket: &str, read: impl FnOnce(Result<Connection>) + Send + 'static) -> Stream {
        let shared = Arc::new(Mutex::new(StreamSocket::default()));
        let stream = Stream(shared.clone());
        let socket = socket.to_string();
        thread::spawn(move || {
            let connection = Connection::open(&socket, None).and_then(|connection| {
                let mut shared = shared.lock().unwrap();
                if shared.closed {
                    return Err(Error::new("The stream was closed"));
                }
                shared.socket = Some(connection.handle()?);
                Ok(connection)
            });
            read(connection);
        });
        stream
    }

    /// Runs `command` and passes its stdout on as it comes. `on_end` gets its stderr when it fails,
    /// and is not called once the stream is dropped.
    pub fn process(mut command: Command, mut on_data: impl FnMut(&[u8]) + Send + 'static, on_end: impl FnOnce(Option<Error>) + Send + 'static) -> Stream {
        let shared = Arc::new(Mutex::new(StreamSocket::default()));
        let stream = Stream(shared.clone());
        thread::spawn(move || {
            let result = (|| {
                let program = command.get_program().to_string_lossy().into_owned();
                let mut child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|error| Error::new(format!("Cannot run {program}: {error}")))?;
                let (mut stdout, mut stderr) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
                {
                    let mut shared = shared.lock().unwrap();
                    if shared.closed {
                        let _ = child.kill();
                    }
                    shared.child = Some(child);
                }
                let mut buffer = [0; 16 * 1024];
                loop {
                    let read = stdout.read(&mut buffer)?;
                    if read == 0 {
                        break;
                    }
                    on_data(&buffer[..read]);
                }
                let mut errors = String::new();
                stderr.read_to_string(&mut errors)?;
                let status = shared.lock().unwrap().child.as_mut().unwrap().wait()?;
                if status.success() {
                    return Ok(());
                }
                Err(Error::new(errors.lines().last().unwrap_or("The process ended.").trim().to_string()))
            })();
            if !shared.lock().unwrap().closed {
                on_end(result.err());
            }
        });
        stream
    }

    /// Calls `tick` every `every` until it fails or the stream is dropped; `on_end` gets the failure.
    pub fn poll(every: Duration, mut tick: impl FnMut() -> Result<()> + Send + 'static, on_end: impl FnOnce(Error) + Send + 'static) -> Stream {
        let shared = Arc::new(Mutex::new(StreamSocket::default()));
        let stream = Stream(shared.clone());
        thread::spawn(move || {
            while !shared.lock().unwrap().closed {
                if let Err(error) = tick() {
                    if !shared.lock().unwrap().closed {
                        on_end(error);
                    }
                    return;
                }
                thread::sleep(every);
            }
        });
        stream
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let mut shared = self.0.lock().unwrap();
        shared.closed = true;
        if let Some(socket) = &shared.socket {
            let _ = socket.shutdown(Shutdown::Both);
        }
        if let Some(child) = &mut shared.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn serve(response: &'static [u8]) -> (String, thread::JoinHandle<Vec<u8>>) {
        let path = std::env::temp_dir().join(format!("dockwatch-http-{}-{:?}.sock", std::process::id(), thread::current().id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = vec![0; 4096];
            let count = stream.read(&mut request).unwrap();
            stream.write_all(response).unwrap();
            request.truncate(count);
            request
        });
        (path.to_string_lossy().into_owned(), server)
    }

    fn exchange(response: &'static [u8]) -> (u16, Vec<u8>, String) {
        let (path, server) = serve(response);
        let mut connection = Connection::open(&path, Some(Duration::from_secs(5))).unwrap();
        connection.send("POST", "/containers/abc/start", None).unwrap();
        let head = connection.read_head().unwrap();
        let mut body = Vec::new();
        connection
            .read_body(&head, |chunk| {
                body.extend_from_slice(chunk);
                Ok(())
            })
            .unwrap();
        let request = String::from_utf8(server.join().unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);
        (head.status, body, request)
    }

    #[test]
    fn reads_a_chunked_body() {
        let (status, body, request) = exchange(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=y\r\nde\r\n0\r\n\r\n");
        assert_eq!(status, 200);
        assert_eq!(body, b"abcde");
        assert!(request.starts_with("POST /containers/abc/start HTTP/1.1\r\n"));
        assert!(request.contains("Content-Length: 0\r\n"));
    }

    #[test]
    fn reads_a_body_of_known_length_and_one_until_eof() {
        let (status, body, _) = exchange(b"HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\n\r\nnopeEXTRA");
        assert_eq!((status, body.as_slice()), (404, &b"nope"[..]));
        let (_, body, _) = exchange(b"HTTP/1.1 200 OK\r\n\r\nuntil the end");
        assert_eq!(body, b"until the end");
    }
}
