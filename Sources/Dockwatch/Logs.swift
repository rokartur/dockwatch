import Foundation

/// Joins chunks into complete lines; a line split across chunks waits for its newline.
struct LineSplitter {
    private var partial: [UInt8] = []

    mutating func feed(_ bytes: some Sequence<UInt8>) -> [String] {
        var lines: [String] = []
        for byte in bytes {
            if byte == UInt8(ascii: "\n") {
                lines.append(String(decoding: partial, as: UTF8.self))
                partial.removeAll(keepingCapacity: true)
            } else {
                partial.append(byte)
            }
        }
        return lines
    }
}

/// Log stream of a container without a TTY: frames of an 8-byte header (stream, 3 zero bytes,
/// big-endian payload size) and the payload. A TTY container sends raw text.
struct LogDecoder {
    private var frames: [UInt8] = []
    private var lines = LineSplitter()

    mutating func feed(_ chunk: Data, contentType: String) -> [LogLine] {
        guard contentType.contains("multiplexed") else {
            return lines.feed(chunk).map(LogLine.init)
        }
        frames += chunk
        var text: [UInt8] = []
        var offset = 0
        while frames.count - offset >= 8 {
            let size = frames[offset + 4 ..< offset + 8].reduce(0) { $0 << 8 | Int($1) }
            guard frames.count - offset - 8 >= size else { break }
            text += frames[offset + 8 ..< offset + 8 + size]
            offset += 8 + size
        }
        frames.removeFirst(offset)
        return lines.feed(text).map(LogLine.init)
    }
}

struct LogLine: Equatable {
    var timestamp: String
    var text: String

    /// Takes `<RFC 3339> <text>` from `docker logs --timestamps`.
    init(_ raw: String) {
        let parts = raw.split(separator: " ", maxSplits: 1, omittingEmptySubsequences: false)
        timestamp = String(parts[0])
        text = stripANSI(parts.count > 1 ? String(parts[1]) : "")
    }

    /// `08:03:46.002` out of `2026-11-04T08:03:46.002417Z`.
    var time: Substring {
        let start = timestamp.index(timestamp.startIndex, offsetBy: 11, limitedBy: timestamp.endIndex) ?? timestamp.endIndex
        return timestamp[start...].prefix(12)
    }
}

struct LogBuffer {
    static let maxLines = 10_000
    private(set) var lines: [LogLine] = []

    /// Appends and drops the oldest lines past `maxLines`; returns the dropped ones.
    mutating func append(_ new: [LogLine]) -> ArraySlice<LogLine> {
        lines += new
        let overflow = max(0, lines.count - Self.maxLines)
        let dropped = lines[..<overflow]
        lines.removeFirst(overflow)
        return dropped
    }

    mutating func clear() {
        lines.removeAll()
    }
}

func logLineMatches(_ line: LogLine, _ query: String) -> Bool {
    query.isEmpty || line.text.range(of: query, options: .caseInsensitive) != nil
}

/// Removes CSI (`ESC [ ... final`), OSC (`ESC ] ... BEL` or `ESC \`), two-byte escapes, and `\r`.
func stripANSI(_ text: String) -> String {
    guard text.unicodeScalars.contains(where: { $0 == "\u{1B}" || $0 == "\r" }) else { return text }
    var out = ""
    var scalars = text.unicodeScalars.makeIterator()
    while let scalar = scalars.next() {
        if scalar == "\r" { continue }
        if scalar != "\u{1B}" {
            out.unicodeScalars.append(scalar)
            continue
        }
        switch scalars.next() {
        case "[":
            while let next = scalars.next(), !(0x40...0x7E).contains(next.value) {}
        case "]":
            var previous: Unicode.Scalar?
            while let next = scalars.next() {
                if next == "\u{07}" || (previous == "\u{1B}" && next == "\\") { break }
                previous = next
            }
        default:
            break
        }
    }
    return out
}
