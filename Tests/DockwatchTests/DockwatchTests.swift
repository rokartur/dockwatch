import AppKit
import Testing
@testable import Dockwatch

@Test func pullDefaultsToLatestOnlyWithoutTagOrDigest() {
    #expect(withDefaultTag("alpine") == "alpine:latest")
    #expect(withDefaultTag("nginx:1.25") == "nginx:1.25")
    #expect(withDefaultTag("localhost:5000/app") == "localhost:5000/app:latest")
    #expect(withDefaultTag("localhost:5000/app:v2") == "localhost:5000/app:v2")
    #expect(withDefaultTag("alpine@sha256:abc") == "alpine@sha256:abc")
}

@Test func untaggedImageGetsOneRowKeyedByID() {
    let rows = imagesFromSummary(ImageSummary(Id: "sha256:1", RepoTags: ["<none>:<none>"], Created: 0, Size: 0))
    #expect(rows.map(\.reference) == ["sha256:1"])
    #expect(rows[0].name == "<none>")
}

@Test func imageRowPerTagSplitsOnLastColon() {
    let rows = imagesFromSummary(ImageSummary(Id: "x", RepoTags: ["localhost:5000/app:v2", "app:latest"], Created: 0, Size: 0))
    #expect(rows.map { "\($0.name) \($0.tag)" } == ["localhost:5000/app v2", "app latest"])
}

@Test func publishedPortsDedupeIPv4AndIPv6() {
    let ports = [
        ContainerSummary.Port(PrivatePort: 80, PublicPort: 8080),
        ContainerSummary.Port(PrivatePort: 80, PublicPort: 8080),
        ContainerSummary.Port(PrivatePort: 5432, PublicPort: 5432),
        ContainerSummary.Port(PrivatePort: 9000, PublicPort: nil),
    ]
    #expect(publishedPorts(ports) == "5432:5432, 8080:80")
}

@Test func contextsGetKnownNamesAndDedupeAgainstKnownPaths() {
    let contexts = [
        (name: "orbstack", host: "unix:///Users/test/.orbstack/run/docker.sock"),
        (name: "lighter", host: "unix:///Users/test/.lighter/docker.sock"),
        (name: "default", host: "unix:///var/run/docker.sock"),
        (name: "remote", host: "ssh://box"),
        (name: "custom", host: "unix:///tmp/custom.sock"),
    ]
    let runtimes = runtimeCandidates(home: "/Users/test", dockerHost: nil, contexts: contexts, currentContext: "lighter") {
        $0.hasSuffix(".orbstack/run/docker.sock") || $0.hasSuffix(".rd/docker.sock")
    }
    #expect(runtimes.map { "\($0.name) \($0.current)" } == [
        "custom false", "Lighter true", "OrbStack false", "Rancher Desktop false",
    ])
}

@Test func dockerHostWinsOverCurrentContext() {
    let socket = "unix:///Users/test/.colima/default/docker.sock"
    let runtimes = runtimeCandidates(
        home: "/Users/test", dockerHost: socket, contexts: [(name: "colima", host: socket)], currentContext: "colima"
    ) { _ in false }
    #expect(runtimes.map { "\($0.name) \($0.current)" } == ["Colima true"])
}

@Test func missingKnownSocketsAreHidden() {
    #expect(runtimeCandidates(home: "/Users/test", dockerHost: nil, contexts: [], currentContext: nil) { _ in false }.isEmpty)
}

private func frame(_ text: String) -> Data {
    let payload = Array(text.utf8)
    let size = UInt32(payload.count).bigEndian
    return Data([1, 0, 0, 0]) + withUnsafeBytes(of: size) { Data($0) } + Data(payload)
}

@Test func logFramesSplitAcrossChunksJoinIntoLines() {
    var decoder = LogDecoder()
    let stream = frame("2026-01-01T08:03:46.002417Z first\n2026-01-01T08:03:47.000000Z sec") + frame("ond\n")
    let type = "application/vnd.docker.multiplexed-stream"
    var lines = decoder.feed(stream.prefix(5), contentType: type)
    lines += decoder.feed(stream.dropFirst(5).prefix(40), contentType: type)
    lines += decoder.feed(stream.dropFirst(45), contentType: type)
    #expect(lines.map(\.text) == ["first", "second"])
    #expect(lines[0].time == "08:03:46.002")
}

@Test func rawLogStreamOfTTYContainer() {
    var decoder = LogDecoder()
    let lines = decoder.feed(Data("2026-01-01T00:00:00Z \u{1B}[32mgreen\u{1B}[0m\r\n".utf8), contentType: "application/vnd.docker.raw-stream")
    #expect(lines.map(\.text) == ["green"])
}

@Test func stripsCSIAndOSCSequences() {
    #expect(stripANSI("\u{1B}[1;31mred\u{1B}[0m") == "red")
    #expect(stripANSI("\u{1B}]0;title\u{07}text") == "text")
    #expect(stripANSI("\u{1B}]8;;http://x\u{1B}\\link") == "link")
    #expect(stripANSI("plain") == "plain")
}

@Test func logBufferKeepsTheLastTenThousandLines() {
    var buffer = LogBuffer()
    let first = buffer.append((0..<LogBuffer.maxLines).map { LogLine("t \($0)") })
    #expect(first.isEmpty)
    let dropped = buffer.append([LogLine("t new")])
    #expect(dropped.map(\.text) == ["0"])
    #expect(buffer.lines.count == LogBuffer.maxLines)
    #expect(buffer.lines.last?.text == "new")
}

private func container(_ name: String, project: String?, running: Bool = true) -> Container {
    Container(id: name, name: name, image: "img", imageID: "i", ports: "", networks: [], status: "", running: running, project: project)
}

@Test func containersGroupByComposeProjectWithOtherLast() {
    let rows = groupedByProject([
        container("solo", project: nil),
        container("web", project: "shop"),
        container("db", project: "shop", running: false),
        container("api", project: "blog"),
    ])
    #expect(rows.map { $0.id ?? "header" } == ["header", "api", "header", "db", "web", "header", "solo"])
    #expect(rows[2] == .group(name: "shop", running: 1, total: 2))
    #expect(rows[5] == .group(name: "Other", running: 1, total: 1))
}

@Test func noHeadersWithoutComposeProjects() {
    let rows = groupedByProject([container("b", project: nil), container("a", project: nil)])
    #expect(rows.map { $0.id ?? "header" } == ["a", "b"])
}

@Test func statsMatchDockerStatsFormulas() throws {
    let json = """
    {"precpu_stats":{"cpu_usage":{"total_usage":1000},"system_cpu_usage":100000,"online_cpus":4},
     "cpu_stats":{"cpu_usage":{"total_usage":6000},"system_cpu_usage":200000,"online_cpus":4},
     "memory_stats":{"usage":300,"limit":1000,"stats":{"inactive_file":100}},
     "networks":{"eth0":{"rx_bytes":10,"tx_bytes":20},"eth1":{"rx_bytes":1,"tx_bytes":2}},
     "blkio_stats":{"io_service_bytes_recursive":[{"op":"read","value":7},{"op":"Write","value":9}]}}
    """
    let sample = Sample(try JSONDecoder().decode(StatsResponse.self, from: Data(json.utf8)))
    // 5_000 / 100_000 * 4 CPUs * 100
    #expect(sample.cpuPercent == 20)
    #expect(sample.memoryUsed == 200)
    #expect(sample.netRx == 11 && sample.netTx == 22)
    #expect(sample.blockRead == 7 && sample.blockWrite == 9)
}

@Test func firstStatsSampleHasNoCPUPercent() throws {
    let json = #"{"cpu_stats":{"cpu_usage":{"total_usage":6000},"system_cpu_usage":200000},"precpu_stats":{"cpu_usage":{}}}"#
    #expect(Sample(try JSONDecoder().decode(StatsResponse.self, from: Data(json.utf8))).cpuPercent == nil)
}

@Test func formatsBytes() {
    #expect(formatBytes(512) == "512 B")
    #expect(formatBytes(1536) == "1.5 KiB")
    #expect(formatBytes(3 * 1024 * 1024 * 1024) == "3.0 GiB")
}

@Test func highlightsJSONKeysStringsKeywordsAndNumbers() {
    let json = #"{ "a\"1" : "x:2", "n" : -1.5, "t" : null }"#
    let result = highlightJSON(json, font: .systemFont(ofSize: 11))
    func color(of token: String) -> NSColor? {
        let location = (json as NSString).range(of: token).location
        return result.attribute(.foregroundColor, at: location, effectiveRange: nil) as? NSColor
    }
    #expect(color(of: #""a\"1""#) == .systemBlue)
    #expect(color(of: " : \"x") == .labelColor)
    #expect(color(of: #""x:2""#) == .systemOrange)
    #expect(color(of: "2\"") == .systemOrange)
    #expect(color(of: "-1.5") == .systemPurple)
    #expect(color(of: "null") == .systemPink)
}
