import Foundation

struct Container: Equatable {
    var id: String
    var name: String
    var image: String
    var imageID: String
    var ports: String
    var networks: [String]
    var status: String
    var running: Bool
    var project: String?
    var service: String? = nil
    /// Services this one waits for, from compose's `com.docker.compose.depends_on` label.
    var dependsOn: [String] = []
}

struct Image: Equatable {
    /// `name:tag`, or the image ID when untagged; one image with two tags gives two rows.
    var reference: String
    var id: String
    var name: String
    var tag: String
    var created: Date
    var size: Int64
}

struct Network: Equatable {
    var id: String
    var name: String
    var driver: String
    var scope: String
    var subnet = ""
    var project: String? = nil

    /// Docker creates these itself and refuses to remove them.
    var isSystem: Bool { ["bridge", "host", "none"].contains(name) }
}

struct Snapshot: Equatable {
    var containers: [Container] = []
    var images: [Image] = []
    var networks: [Network] = []
}

struct Engine {
    var version: String
    var cpus: Int
    var memory: Int64
    var system: String
    var architecture: String
}

/// Docker Engine API client over a Unix socket. Paths carry no API version, so the daemon
/// answers in its own latest one.
final class Docker {
    let socket: String

    init(socket: String) {
        self.socket = socket
    }

    func snapshot() async throws -> Snapshot {
        async let containers = get("/containers/json?all=1", as: [ContainerSummary].self)
        async let images = get("/images/json", as: [ImageSummary].self)
        async let networks = get("/networks", as: [NetworkSummary].self)
        return Snapshot(
            containers: try await containers.map(Container.init),
            images: try await images.flatMap(imagesFromSummary),
            // The daemon lists networks in map order, which changes per call and would fail the Snapshot comparison.
            networks: try await networks.map {
                Network(
                    id: $0.Id, name: $0.Name, driver: $0.Driver, scope: $0.Scope,
                    subnet: $0.IPAM?.Config?.first?.Subnet ?? "",
                    project: $0.Labels?["com.docker.compose.project"]
                )
            }.sorted { $0.id < $1.id }
        )
    }

    func engine() async throws -> Engine {
        let info = try await get("/info", as: Info.self)
        return Engine(
            version: info.ServerVersion, cpus: info.NCPU, memory: info.MemTotal,
            system: info.OperatingSystem, architecture: info.Architecture
        )
    }

    func ping() async -> Bool {
        let body = try? await request("GET", "/_ping", timeout: 2)
        return body == Data("OK".utf8)
    }

    func start(_ id: String) async throws { try await request("POST", "/containers/\(id)/start") }
    func stop(_ id: String) async throws { try await request("POST", "/containers/\(id)/stop") }
    func restart(_ id: String) async throws { try await request("POST", "/containers/\(id)/restart") }

    /// Volumes always stay: `v=0`.
    func removeContainer(_ id: String, force: Bool) async throws {
        try await request("DELETE", "/containers/\(id)?force=\(force ? 1 : 0)&v=0")
    }

    /// Removing by `name:tag` untags; the image goes once its last tag does.
    func removeImage(_ reference: String) async throws {
        try await request("DELETE", "/images/\(escape(reference))")
    }

    func removeNetwork(_ id: String) async throws {
        try await request("DELETE", "/networks/\(id)")
    }

    /// Removes every image no container uses, like `docker image prune -a`. Returns bytes reclaimed.
    func pruneImages() async throws -> Int64 {
        let filters = escape(#"{"dangling":["false"]}"#)
        struct Report: Decodable { let SpaceReclaimed: Int64? }
        return try await post("/images/prune?filters=\(filters)", as: Report.self).SpaceReclaimed ?? 0
    }

    /// Returns how many networks were removed.
    func pruneNetworks() async throws -> Int {
        struct Report: Decodable { let NetworksDeleted: [String]? }
        return try await post("/networks/prune", as: Report.self).NetworksDeleted?.count ?? 0
    }

    func pull(_ reference: String) async throws {
        let body = try await request("POST", "/images/create?fromImage=\(escape(withDefaultTag(reference)))")
        // Pull failures arrive as a progress line with an `error` field under status 200.
        struct Progress: Decodable { let error: String? }
        for line in body.split(separator: UInt8(ascii: "\n")) {
            if let message = try? JSONDecoder().decode(Progress.self, from: Data(line)).error {
                throw DockerError(message: message)
            }
        }
    }

    func inspect(_ section: Section, _ id: String) async throws -> Any {
        let path = switch section {
        case .containers: "/containers/\(id)/json"
        case .images: "/images/\(escape(id))/json"
        case .networks: "/networks/\(id)"
        }
        return try JSONSerialization.jsonObject(with: try await request("GET", path))
    }

    /// Container, image and network events, one call per chunk; `onEnd` means the runtime went away.
    func events(onEvent: @escaping () -> Void, onEnd: @escaping (Error?) -> Void) throws -> DockerStream {
        let filters = escape(#"{"type":["container","image","network"]}"#)
        return try stream("/events?filters=\(filters)", onData: { _, _ in onEvent() }, onEnd: onEnd)
    }

    /// Last 1000 lines, then follows. Each line starts with Docker's RFC 3339 timestamp.
    func logs(
        _ id: String, onData: @escaping (Data, String) -> Void, onEnd: @escaping (Error?) -> Void
    ) throws -> DockerStream {
        let path = "/containers/\(id)/logs?follow=1&stdout=1&stderr=1&timestamps=1&tail=1000"
        return try stream(path, onData: onData, onEnd: onEnd)
    }

    func stats(
        _ id: String, onData: @escaping (Data, String) -> Void, onEnd: @escaping (Error?) -> Void
    ) throws -> DockerStream {
        try stream("/containers/\(id)/stats?stream=1", onData: onData, onEnd: onEnd)
    }

    /// One reading with `precpu_stats` filled, so CPU needs no second call; the daemon takes about 1 s.
    func statsOnce(_ id: String) async throws -> Sample {
        Sample(try await get("/containers/\(id)/stats?stream=false", as: StatsResponse.self))
    }

    /// Runs `command` without a TTY and waits for it to exit. No shell: arguments pass as they are.
    func exec(_ id: String, _ command: [String]) async throws -> (output: Data, errors: Data) {
        let create = try JSONSerialization.data(withJSONObject: ["AttachStdout": true, "AttachStderr": true, "Cmd": command])
        struct Created: Decodable { let Id: String }
        let exec = try JSONDecoder().decode(Created.self, from: try await request("POST", "/containers/\(id)/exec", body: create))
        let start = try JSONSerialization.data(withJSONObject: ["Detach": false, "Tty": false])
        return demultiplex(try await request("POST", "/exec/\(exec.Id)/start", body: start))
    }

    /// Entries of one folder in a running container, through `find` and `stat` in the image.
    func listFiles(_ id: String, in folder: String) async throws -> [FileEntry] {
        let output: Data, errors: Data
        do {
            (output, errors) = try await exec(id, [
                "find", folder, "-mindepth", "1", "-maxdepth", "1",
                "-exec", "stat", "-L", "-c", "%F\t%s\t%Y\t%A\t%n", "{}", "+",
            ])
        } catch let error as DockerError where error.status == 409 {
            throw DockerError(message: "Start the container to browse its files.", status: 409)
        }
        // Broken links make stat fail for one entry while the rest still list, so stderr counts only alone.
        if output.isEmpty, !errors.isEmpty {
            throw DockerError(message: String(decoding: errors, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines))
        }
        return parseListing(String(decoding: output, as: UTF8.self))
    }

    /// A file or folder as Docker sends it: a tar archive. Works on stopped containers too.
    func archive(_ id: String, path: String) async throws -> Data {
        try await request("GET", "/containers/\(id)/archive?path=\(escape(path))")
    }

    /// Streams the archive into `file`, so a large download never sits in memory.
    func downloadArchive(_ id: String, path: String, to file: URL) async throws {
        FileManager.default.createFile(atPath: file.path, contents: nil)
        let handle = try FileHandle(forWritingTo: file)
        defer { try? handle.close() }
        try await request("GET", "/containers/\(id)/archive?path=\(escape(path))", body: nil, contentType: "", timeout: 120) {
            try handle.write(contentsOf: $0)
        }
    }

    /// Extracts a tar archive into `folder` inside the container.
    func uploadArchive(_ id: String, tar: Data, into folder: String) async throws {
        try await request("PUT", "/containers/\(id)/archive?path=\(escape(folder))", body: tar, contentType: "application/x-tar")
    }

    private func stream(
        _ path: String, onData: @escaping (Data, String) -> Void, onEnd: @escaping (Error?) -> Void
    ) throws -> DockerStream {
        let connection = try HTTPConnection(socket: socket, timeout: nil)
        try connection.send("GET", path, body: nil)
        return DockerStream(connection: connection, onData: onData, onEnd: onEnd)
    }

    private func get<T: Decodable>(_ path: String, as type: T.Type) async throws -> T {
        try JSONDecoder().decode(type, from: try await request("GET", path))
    }

    private func post<T: Decodable>(_ path: String, as type: T.Type) async throws -> T {
        try JSONDecoder().decode(type, from: try await request("POST", path))
    }

    /// Stop waits up to 10 s before SIGKILL until the daemon answers, so the default timeout must exceed it.
    @discardableResult
    private func request(
        _ method: String, _ path: String, body: Data? = nil, contentType: String = "application/json",
        timeout: TimeInterval = 120
    ) async throws -> Data {
        var response = Data()
        try await request(method, path, body: body, contentType: contentType, timeout: timeout) { response.append($0) }
        return response
    }

    /// Hands the response body to `onChunk` on a background thread as it arrives.
    private func request(
        _ method: String, _ path: String, body: Data?, contentType: String, timeout: TimeInterval,
        onChunk: @escaping (Data) throws -> Void
    ) async throws {
        let socket = socket
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            DispatchQueue.global().async {
                continuation.resume(with: Result {
                    let connection = try HTTPConnection(socket: socket, timeout: timeout)
                    try connection.send(method, path, body: body, contentType: contentType)
                    try connection.readHead()
                    guard connection.status < 400 else {
                        var error = Data()
                        try connection.readBody { error.append($0) }
                        throw Docker.error(status: connection.status, body: error)
                    }
                    try connection.readBody(onChunk)
                })
            }
        }
    }

    static func error(status: Int, body: Data) -> DockerError {
        struct Message: Decodable { let message: String }
        let message = try? JSONDecoder().decode(Message.self, from: body).message
        return DockerError(message: message ?? "Docker answered HTTP \(status)", status: status)
    }

    private func escape(_ value: String) -> String {
        var allowed = CharacterSet.urlQueryAllowed
        allowed.remove(charactersIn: "&=+?#")
        return value.addingPercentEncoding(withAllowedCharacters: allowed) ?? value
    }
}

/// `4f1d2c3b5a69` out of `sha256:4f1d2c3b5a69...`, as the docker CLI shows IDs.
func shortID(_ id: String) -> String {
    String((id.hasPrefix("sha256:") ? id.dropFirst(7) : Substring(id)).prefix(12))
}

/// The Engine API pulls every tag when none is given; the docker CLI means `latest`.
func withDefaultTag(_ reference: String) -> String {
    let lastSegment = reference.split(separator: "/").last ?? ""
    if reference.contains("@") || lastSegment.contains(":") {
        return reference
    }
    return reference + ":latest"
}

func imagesFromSummary(_ summary: ImageSummary) -> [Image] {
    func image(_ reference: String, _ name: String, _ tag: String) -> Image {
        Image(
            reference: reference, id: summary.Id, name: name, tag: tag,
            created: Date(timeIntervalSince1970: TimeInterval(summary.Created)), size: summary.Size
        )
    }
    var images: [Image] = []
    for reference in summary.RepoTags ?? [] where reference != "<none>:<none>" {
        guard let colon = reference.lastIndex(of: ":") else { continue }
        images.append(image(reference, String(reference[..<colon]), String(reference[reference.index(after: colon)...])))
    }
    if images.isEmpty {
        images.append(image(summary.Id, "<none>", "<none>"))
    }
    return images
}

/// `8080:80, 5432:5432`, published ports only; IPv4 and IPv6 bindings of one port appear once.
func publishedPorts(_ ports: [ContainerSummary.Port]) -> String {
    var pairs: [String] = []
    for port in ports.sorted(by: { ($0.PublicPort ?? 0, $0.PrivatePort) < ($1.PublicPort ?? 0, $1.PrivatePort) }) {
        guard let published = port.PublicPort else { continue }
        let pair = "\(published):\(port.PrivatePort)"
        if pairs.last != pair { pairs.append(pair) }
    }
    return pairs.joined(separator: ", ")
}

extension Container {
    init(_ summary: ContainerSummary) {
        let name = summary.Names?.first.map { String($0.drop(while: { $0 == "/" })) } ?? ""
        self.init(
            id: summary.Id, name: name, image: summary.Image, imageID: summary.ImageID,
            ports: publishedPorts(summary.Ports ?? []),
            networks: summary.NetworkSettings?.Networks.map { Array($0.keys) } ?? [],
            status: summary.Status,
            running: summary.State == "running", project: summary.Labels?["com.docker.compose.project"],
            service: summary.Labels?["com.docker.compose.service"],
            // "db:service_started:false,redis:service_healthy:true"
            dependsOn: (summary.Labels?["com.docker.compose.depends_on"] ?? "").split(separator: ",").compactMap {
                $0.split(separator: ":").first.map(String.init)
            }
        )
    }
}

/// Compose start order: a batch depends only on earlier batches, so each batch can run at once.
/// A dependency outside `containers` counts as met; members of a cycle go last, together.
func startBatches(_ containers: [Container]) -> [[Container]] {
    let services = Set(containers.compactMap(\.service))
    var left = containers
    var started: Set<String> = []
    var batches: [[Container]] = []
    while !left.isEmpty {
        let ready = left.filter { container in
            container.dependsOn.allSatisfy { started.contains($0) || !services.contains($0) }
        }
        if ready.isEmpty {
            batches.append(left)
            break
        }
        batches.append(ready)
        started.formUnion(ready.compactMap(\.service))
        left.removeAll { container in ready.contains { $0.id == container.id } }
    }
    return batches
}

// Engine API shapes, named as Docker names its JSON fields.

struct ContainerSummary: Decodable {
    struct Port: Decodable {
        var PrivatePort: Int
        var PublicPort: Int?
    }
    var Id: String
    var Names: [String]?
    var Image: String
    var ImageID: String
    struct Settings: Decodable {
        struct Endpoint: Decodable {}
        var Networks: [String: Endpoint]?
    }
    var Ports: [Port]?
    var NetworkSettings: Settings?
    var Status: String
    var State: String
    var Labels: [String: String]?
}

struct ImageSummary: Decodable {
    var Id: String
    var RepoTags: [String]?
    var Created: Int64
    var Size: Int64
}

private struct NetworkSummary: Decodable {
    var Id: String
    var Name: String
    var Driver: String
    var Scope: String
    var IPAM: IPAMSummary?
    var Labels: [String: String]?

    struct IPAMSummary: Decodable {
        var Config: [Pool]?
        struct Pool: Decodable { var Subnet: String? }
    }
}

private struct Info: Decodable {
    var ServerVersion: String
    var NCPU: Int
    var MemTotal: Int64
    var OperatingSystem: String
    var Architecture: String
}
