import Foundation

struct DockerError: LocalizedError {
    let message: String
    var status = 0
    var errorDescription: String? { message }
}

/// One HTTP/1.1 exchange over a Unix socket. Requests send `Connection: close`, so EOF ends a body
/// that has neither chunked encoding nor a length.
final class HTTPConnection {
    private let fd: Int32
    private var buffer: [UInt8] = []
    private(set) var status = 0
    private(set) var headers: [String: String] = [:]

    /// `timeout` bounds each read and write; nil waits forever, for streams.
    init(socket path: String, timeout: TimeInterval?) throws {
        fd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw DockerError(message: "socket(): \(String(cString: strerror(errno)))") }
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let pathBytes = Array(path.utf8)
        guard pathBytes.count < MemoryLayout.size(ofValue: address.sun_path) else {
            Darwin.close(fd)
            throw DockerError(message: "Socket path too long: \(path)")
        }
        withUnsafeMutableBytes(of: &address.sun_path) { $0.copyBytes(from: pathBytes) }
        let connected = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        if connected != 0 {
            let reason = String(cString: strerror(errno))
            Darwin.close(fd)
            throw DockerError(message: "Cannot connect to \(path): \(reason)")
        }
        var on: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &on, socklen_t(MemoryLayout<Int32>.size))
        if let timeout {
            var limit = timeval(tv_sec: Int(timeout), tv_usec: 0)
            setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &limit, socklen_t(MemoryLayout<timeval>.size))
            setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &limit, socklen_t(MemoryLayout<timeval>.size))
        }
    }

    /// The descriptor closes only here, so a thread still reading never sees a reused number.
    deinit { Darwin.close(fd) }

    /// Safe from any thread: wakes a blocked read, which then sees EOF.
    func interrupt() {
        shutdown(fd, SHUT_RDWR)
    }

    func send(_ method: String, _ path: String, body: Data?, contentType: String = "application/json") throws {
        var head = "\(method) \(path) HTTP/1.1\r\nHost: docker\r\nConnection: close\r\n"
        if let body {
            head += "Content-Type: \(contentType)\r\nContent-Length: \(body.count)\r\n"
        } else if method != "GET" {
            head += "Content-Length: 0\r\n"
        }
        head += "\r\n"
        var bytes = Array(head.utf8)
        bytes += body ?? Data()
        var sent = 0
        while sent < bytes.count {
            let count = bytes[sent...].withUnsafeBytes { Darwin.write(fd, $0.baseAddress, $0.count) }
            if count < 0 && errno == EINTR { continue }
            guard count > 0 else { throw DockerError(message: "Write to Docker failed: \(String(cString: strerror(errno)))") }
            sent += count
        }
    }

    func readHead() throws {
        let statusLine = try readLine()
        let parts = statusLine.split(separator: " ")
        guard parts.count >= 2, let code = Int(parts[1]) else {
            throw DockerError(message: "Bad HTTP status line: \(statusLine)")
        }
        status = code
        while true {
            let line = try readLine()
            if line.isEmpty { return }
            guard let colon = line.firstIndex(of: ":") else { continue }
            let name = line[..<colon].lowercased()
            headers[name] = line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces)
        }
    }

    /// Hands the body to `onChunk` as it arrives: per HTTP chunk, or per read without chunking.
    func readBody(_ onChunk: (Data) throws -> Void) throws {
        if headers["transfer-encoding"]?.lowercased() == "chunked" {
            while true {
                let sizeLine = try readLine()
                guard let size = Int(sizeLine.split(separator: ";").first ?? "", radix: 16) else {
                    throw DockerError(message: "Bad chunk size: \(sizeLine)")
                }
                if size == 0 { return }
                try onChunk(Data(try readExactly(size)))
                _ = try readLine()
            }
        }
        if let length = headers["content-length"].flatMap(Int.init) {
            try onChunk(Data(try readExactly(length)))
            return
        }
        while true {
            if !buffer.isEmpty {
                try onChunk(Data(buffer))
                buffer.removeAll(keepingCapacity: true)
            }
            guard try fill() else { return }
        }
    }

    private func readLine() throws -> String {
        while true {
            if let end = buffer.firstIndex(of: UInt8(ascii: "\n")) {
                var line = buffer[..<end]
                if line.last == UInt8(ascii: "\r") { line = line.dropLast() }
                let text = String(decoding: line, as: UTF8.self)
                buffer.removeFirst(end + 1)
                return text
            }
            guard try fill() else { throw DockerError(message: "Docker closed the connection") }
        }
    }

    private func readExactly(_ count: Int) throws -> [UInt8] {
        while buffer.count < count {
            guard try fill() else { throw DockerError(message: "Docker closed the connection") }
        }
        let bytes = Array(buffer[..<count])
        buffer.removeFirst(count)
        return bytes
    }

    /// Appends one read to the buffer; false at EOF.
    private func fill() throws -> Bool {
        var chunk = [UInt8](repeating: 0, count: 64 * 1024)
        while true {
            let count = chunk.withUnsafeMutableBytes { Darwin.read(fd, $0.baseAddress, $0.count) }
            if count > 0 {
                buffer += chunk[..<count]
                return true
            }
            if count == 0 { return false }
            if errno == EINTR { continue }
            if errno == EAGAIN { throw DockerError(message: "Docker did not answer in time") }
            if errno == ENOTCONN { return false }
            throw DockerError(message: "Read from Docker failed: \(String(cString: strerror(errno)))")
        }
    }
}

/// A long-lived response read on its own thread. Callbacks run on the main queue and stop
/// once `cancel()`, called on the main queue, returns.
final class DockerStream {
    private let connection: HTTPConnection
    private var cancelled = false

    init(
        connection: HTTPConnection,
        onData: @escaping (_ chunk: Data, _ contentType: String) -> Void,
        onEnd: @escaping (Error?) -> Void
    ) {
        self.connection = connection
        Thread { [self] in
            do {
                try connection.readHead()
                let contentType = connection.headers["content-type"] ?? ""
                guard connection.status < 400 else {
                    var body = Data()
                    try connection.readBody { body.append($0) }
                    throw Docker.error(status: connection.status, body: body)
                }
                try connection.readBody { chunk in
                    DispatchQueue.main.async { if !self.cancelled { onData(chunk, contentType) } }
                }
                DispatchQueue.main.async { if !self.cancelled { onEnd(nil) } }
            } catch {
                DispatchQueue.main.async { if !self.cancelled { onEnd(error) } }
            }
        }.start()
    }

    func cancel() {
        cancelled = true
        connection.interrupt()
    }
}
