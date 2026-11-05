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

@Test func containersGroupByComposeProjectWithLooseOnesLast() {
    let db = container("db", project: "shop", running: false)
    let web = container("web", project: "shop")
    let rows = groupedByProject([container("solo", project: nil), web, db, container("api", project: "blog")])
    #expect(rows.map { $0.id ?? "header" } == ["header", "api", "header", "db", "web", "solo"])
    #expect(rows[2] == .group(project: "shop", members: [db, web], collapsed: false))
}

@Test func collapsedProjectKeepsItsHeaderAndMembers() {
    let web = container("web", project: "shop")
    let rows = groupedByProject([web, container("api", project: "blog")], collapsed: ["shop"])
    #expect(rows.map { $0.id ?? "header" } == ["header", "api", "header"])
    #expect(rows[2] == .group(project: "shop", members: [web], collapsed: true))
}

@Test func noHeadersWithoutComposeProjects() {
    let rows = groupedByProject([container("b", project: nil), container("a", project: nil)])
    #expect(rows.map { $0.id ?? "header" } == ["a", "b"])
}

@Test func imagesSplitIntoInUseAndUnusedBySizeCountingSharedBytesOnce() {
    func image(_ reference: String, id: String, size: Int64) -> Image {
        Image(reference: reference, id: id, name: reference, tag: "", created: .now, size: size)
    }
    var web = container("web", project: nil)
    web.imageID = "a"
    let big = image("big", id: "b", size: 3_000_000)
    let tagged = image("app:v1", id: "a", size: 1_000_000)
    let alias = image("app:latest", id: "a", size: 1_000_000)
    let rows = listRows(Snapshot(containers: [web], images: [big, tagged, alias]), section: .images, onlyRunning: false, search: "")
    #expect(rows == [
        .header(title: "In use", detail: "2 images · \(formatBytes(1_000_000))"),
        .image(alias, usedBy: ["web"]),
        .image(tagged, usedBy: ["web"]),
        .header(title: "Unused", detail: "1 image · \(formatBytes(3_000_000))"),
        .image(big, usedBy: []),
    ])
}

@Test func sidebarSummariesCountRunningImagesInUseAndAttachedNetworks() {
    var web = container("web", project: nil)
    web.imageID = "a"
    web.networks = ["app"]
    let snapshot = Snapshot(
        containers: [web, container("db", project: nil, running: false)],
        images: [
            Image(reference: "app:v1", id: "a", name: "app", tag: "v1", created: .now, size: 1_000_000),
            Image(reference: "app:latest", id: "a", name: "app", tag: "latest", created: .now, size: 1_000_000),
            Image(reference: "big", id: "b", name: "big", tag: "", created: .now, size: 3_000_000),
        ],
        networks: [Network(id: "1", name: "app", driver: "bridge", scope: "local"), Network(id: "2", name: "host", driver: "host", scope: "local")]
    )
    #expect(sectionSummary(snapshot, .containers) == "1 running · 1 stopped")
    #expect(sectionSummary(snapshot, .images) == "\(formatBytes(4_000_000)) · 2 in use")
    #expect(sectionSummary(snapshot, .networks) == "1 in use")
}

@Test func networksWithoutAttachedContainersGoUnderUnused() {
    var web = container("web", project: nil)
    web.networks = ["app"]
    let app = Network(id: "1", name: "app", driver: "bridge", scope: "local")
    let host = Network(id: "2", name: "host", driver: "host", scope: "local")
    let rows = listRows(Snapshot(containers: [web], networks: [host, app]), section: .networks, onlyRunning: false, search: "")
    #expect(rows == [
        .header(title: "In use", detail: "1 network"),
        .network(app, attached: ["web"]),
        .header(title: "Unused", detail: "1 network"),
        .network(host, attached: []),
    ])
    let unused = listRows(Snapshot(containers: [web], networks: [host, app]), section: .networks, onlyRunning: false, search: "", onlyUnused: true)
    #expect(unused == [.header(title: "Unused", detail: "1 network"), .network(host, attached: [])])
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

@Test func listingPutsFoldersFirstAndSkipsBadLines() {
    let text = "regular file\t174\t1700000000\t-rw-r--r--\t/etc/hosts\n"
        + "directory\t4096\t1700000000\tdrwxr-xr-x\t/etc/apk\n"
        + "stat: can't stat '/etc/broken': No such file\n"
        + "regular empty file\t0\t1700000000\t-rw-r--r--\t/etc/a b\tc\n"
    let entries = parseListing(text)
    #expect(entries.map(\.name) == ["apk", "a b\tc", "hosts"])
    #expect(entries[0].isFolder && !entries[2].isFolder)
    #expect(entries[2].size == 174 && entries[2].mode == "-rw-r--r--")
}

@Test func demultiplexSplitsStdoutAndStderr() {
    func frame(_ stream: UInt8, _ text: String) -> [UInt8] {
        let payload = Array(text.utf8)
        return [stream, 0, 0, 0, 0, 0, 0, UInt8(payload.count)] + payload
    }
    let result = demultiplex(Data(frame(1, "out1 ") + frame(2, "err") + frame(1, "out2")))
    #expect(String(decoding: result.output, as: UTF8.self) == "out1 out2")
    #expect(String(decoding: result.errors, as: UTF8.self) == "err")
}

@Test func firstFileInTarSkipsLongNameHeaders() throws {
    let folder = FileManager.default.temporaryDirectory.appendingPathComponent("dwtar-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: folder) }
    let name = String(repeating: "n", count: 150) + "-zażółć.txt"
    try Data("hello\n".utf8).write(to: folder.appendingPathComponent(name))
    let tar = folder.appendingPathComponent("out.tar")
    let process = Process()
    process.executableURL = URL(fileURLWithPath: "/usr/bin/tar")
    process.arguments = ["-cf", tar.path, "--format", "pax", "--no-mac-metadata", "-C", folder.path, name]
    try process.run()
    process.waitUntilExit()
    #expect(firstFileInTar(try Data(contentsOf: tar)) == Data("hello\n".utf8))
    #expect(firstFileInTar(Data(count: 1024)) == nil)
}

@Test func uploadArchiveDropsMacExtendedAttributes() throws {
    let folder = FileManager.default.temporaryDirectory.appendingPathComponent("dwup-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: folder) }
    let file = folder.appendingPathComponent("note.txt")
    try Data("hi\n".utf8).write(to: file)
    #expect(setxattr(file.path, "com.example.tag", "x", 1, 0, 0) == 0)
    let archive = folder.appendingPathComponent("out.tar")
    try makeUploadArchive(of: file, at: archive)
    let tar = try Data(contentsOf: archive)
    #expect(tar.range(of: Data("com.example.tag".utf8)) == nil)
    #expect(firstFileInTar(tar) == Data("hi\n".utf8))
}

@Test func parentFolderStopsAtRoot() {
    #expect(parentFolder("/etc/apk") == "/etc")
    #expect(parentFolder("/etc") == "/")
    #expect(parentFolder("/") == "/")
}

@Test func terminalScriptQuotesSocketAndID() {
    let script = terminalScript(socket: "/Users/o'neil/.lighter/docker.sock", containerID: "abc123")
    #expect(script.contains(#"export DOCKER_HOST='unix:///Users/o'\''neil/.lighter/docker.sock'"#))
    #expect(script.contains("exec docker exec -it 'abc123' sh -c"))
    #expect(script.hasPrefix("#!/bin/sh\nrm -f \"$0\"\n"))
}
