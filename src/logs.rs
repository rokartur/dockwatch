use crate::files;
use std::collections::VecDeque;

#[derive(Clone, Debug, PartialEq)]
pub struct LogLine {
    /// RFC 3339 with fixed nanoseconds, as `docker logs --timestamps` sends it, so text order is time order.
    pub timestamp: String,
    pub text: String,
    /// Compose service the line came from, in a project's merged log.
    pub source: Option<String>,
}

impl LogLine {
    pub fn parse(raw: &str) -> Self {
        let (timestamp, text) = raw.split_once(' ').unwrap_or((raw, ""));
        LogLine { timestamp: timestamp.to_string(), text: strip_ansi(text), source: None }
    }

    /// `08:03:46.002` out of `2026-11-04T08:03:46.002417000Z`.
    pub fn time(&self) -> &str {
        self.timestamp.get(11..23).unwrap_or("")
    }
}

/// Log stream of a container without a TTY: frames of an 8-byte header (stream, 3 zero bytes,
/// big-endian payload size) and the payload. A TTY container sends raw text. Stdout and stderr
/// interleave mid-line, so each keeps its own partial line.
#[derive(Default)]
pub struct LogDecoder {
    frames: Vec<u8>,
    multiplexed: Option<bool>,
    stdout: LineSplitter,
    stderr: LineSplitter,
}

impl LogDecoder {
    pub fn feed(&mut self, chunk: &[u8], content_type: &str) -> Vec<LogLine> {
        // Before API 1.42 Docker labels multiplexed logs `raw-stream` too; a frame starts with 0, 1 or 2 and three zeros.
        let multiplexed = *self.multiplexed.get_or_insert_with(|| content_type.contains("multiplexed") || matches!(chunk, [0..=2, 0, 0, 0, ..]));
        if !multiplexed {
            return parse_lines(self.stdout.feed(chunk));
        }
        self.frames.extend_from_slice(chunk);
        let mut lines = Vec::new();
        let mut offset = 0;
        while let Some((is_stderr, size)) = files::frame_header(&self.frames[offset..]) {
            if self.frames.len() - offset - 8 < size {
                break;
            }
            let payload = &self.frames[offset + 8..offset + 8 + size];
            let splitter = if is_stderr { &mut self.stderr } else { &mut self.stdout };
            lines.extend(parse_lines(splitter.feed(payload)));
            offset += 8 + size;
        }
        self.frames.drain(..offset);
        lines
    }
}

fn parse_lines(lines: Vec<String>) -> Vec<LogLine> {
    lines.iter().map(|line| LogLine::parse(line)).collect()
}

/// Joins chunks into complete lines. A line split across chunks waits in `partial` for its newline,
/// up to `MAX_LINE_BYTES`, so a stream of `\r` progress bars or binary still ends lines.
#[derive(Default)]
pub struct LineSplitter {
    partial: Vec<u8>,
}

const MAX_LINE_BYTES: usize = 64 * 1024;

impl LineSplitter {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        for &byte in bytes {
            if byte != b'\n' {
                self.partial.push(byte);
            }
            if byte == b'\n' || self.partial.len() >= MAX_LINE_BYTES {
                lines.push(String::from_utf8_lossy(&self.partial).into_owned());
                self.partial.clear();
            }
        }
        lines
    }
}

const MAX_LINES: usize = 10_000;

/// Appends `new` in timestamp order and drops the oldest past `MAX_LINES`; returns how many were dropped.
/// Lines of one container arrive in order, so each insert scans back only past newer lines of other containers.
pub fn merge(lines: &mut VecDeque<LogLine>, new: Vec<LogLine>) -> usize {
    for line in new {
        let mut at = lines.len();
        while at > 0 && lines[at - 1].timestamp > line.timestamp {
            at -= 1;
        }
        lines.insert(at, line);
    }
    let overflow = lines.len().saturating_sub(MAX_LINES);
    lines.drain(..overflow);
    overflow
}

/// `query` must be lowercase already; callers lowercase it once per pass over the lines.
pub fn matches(line: &LogLine, query: &str) -> bool {
    query.is_empty() || line.text.to_lowercase().contains(query)
}

/// Removes CSI (`ESC [ ... final`), OSC (`ESC ] ... BEL` or `ESC \`), two-byte escapes, `\r` and other
/// control characters, and turns tabs into spaces, so a line renders at its measured width.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => for _ in chars.by_ref().take_while(|next| !('\u{40}'..='\u{7e}').contains(next)) {},
                Some(']') => {
                    let mut previous = None;
                    for next in chars.by_ref() {
                        if next == '\u{07}' || (previous == Some('\u{1b}') && next == '\\') {
                            break;
                        }
                        previous = Some(next);
                    }
                }
                _ => {}
            },
            '\t' => out.push_str("    "),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(stream: u8, payload: &str) -> Vec<u8> {
        let mut bytes = vec![stream, 0, 0, 0];
        bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(payload.as_bytes());
        bytes
    }

    #[test]
    fn decodes_frames_split_across_chunks() {
        let mut stream = frame(1, "2026-11-04T08:03:46.002417000Z hello\n2026-11-04T08:03:47.000000000Z wor");
        stream.extend(frame(2, "2026-11-04T08:03:48.000000000Z oops\n"));
        stream.extend(frame(1, "ld\n"));
        let mut decoder = LogDecoder::default();
        let mut lines = decoder.feed(&stream[..5], "application/vnd.docker.multiplexed-stream");
        assert!(lines.is_empty());
        lines = decoder.feed(&stream[5..], "application/vnd.docker.multiplexed-stream");
        let texts: Vec<_> = lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(texts, ["hello", "oops", "world"]);
        assert_eq!(lines[0].time(), "08:03:46.002");
    }

    #[test]
    fn reads_raw_text_from_a_tty_container() {
        let mut decoder = LogDecoder::default();
        let lines = decoder.feed(b"2026-11-04T08:03:46.000000000Z \x1b[32mgreen\x1b[0m\r\n", "application/vnd.docker.raw-stream");
        assert_eq!(lines[0].text, "green");
    }

    #[test]
    fn finds_frames_behind_an_old_raw_stream_label() {
        let mut decoder = LogDecoder::default();
        let lines = decoder.feed(&frame(1, "2026-11-04T08:03:46.000000000Z hi\n"), "application/vnd.docker.raw-stream");
        assert_eq!((lines[0].time(), lines[0].text.as_str()), ("08:03:46.000", "hi"));
    }

    #[test]
    fn ends_a_line_that_never_gets_a_newline() {
        let mut splitter = LineSplitter::default();
        assert_eq!(splitter.feed(&vec![b'#'; MAX_LINE_BYTES + 10]).len(), 1);
        assert_eq!(splitter.feed(b"\n"), ["#".repeat(10)]);
    }

    #[test]
    fn strips_escapes_and_controls() {
        assert_eq!(strip_ansi("a\x1b[1;31mb\x1b]0;title\x07c\x1b]8;;x\x1b\\d\te"), "abcd    e");
    }

    #[test]
    fn merges_by_timestamp_and_caps() {
        let line = |timestamp: &str| LogLine { timestamp: timestamp.into(), text: String::new(), source: None };
        let mut lines = VecDeque::from([line("2026-01-01T00:00:01.000000000Z"), line("2026-01-01T00:00:03.000000000Z")]);
        merge(&mut lines, vec![line("2026-01-01T00:00:02.000000000Z"), line("2026-01-01T00:00:04.000000000Z")]);
        let order: Vec<_> = lines.iter().map(|line| &line.timestamp[17..19]).collect();
        assert_eq!(order, ["01", "02", "03", "04"]);
        let dropped = merge(&mut lines, vec![line("2026-01-01T00:00:05.000000000Z"); MAX_LINES]);
        assert_eq!((dropped, lines.len()), (4, MAX_LINES));
    }
}
