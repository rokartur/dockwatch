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
            networks: try await networks.map {
                Network(id: $0.Id, name: $0.Name, driver: $0.Driver, scope: $0.Scope)
            }
        )
    }

    func engine() async throws -> Engine {
        let info = try await get("/info", as: Info.self)
        return Engine(version: info.ServerVersion, cpus: info.NCPU, memory: info.MemTotal)
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
    private func request(_ method: String, _ path: String, timeout: TimeInterval = 120) async throws -> Data {
        let socket = socket
        return try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global().async {
                continuation.resume(with: Result {
                    let connection = try HTTPConnection(socket: socket, timeout: timeout)
                    try connection.send(method, path, body: nil)
                    try connection.readHead()
                    var body = Data()
                    try connection.readBody { body.append($0) }
                    guard connection.status < 400 else {
                        throw Docker.error(status: connection.status, body: body)
                    }
                    return body
                })
            }
        }
    }

    static func error(status: Int, body: Data) -> DockerError {
        struct Message: Decodable { let message: String }
        let message = try? JSONDecoder().decode(Message.self, from: body).message
        return DockerError(message: message ?? "Docker answered HTTP \(status)")
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
            running: summary.State == "running", project: summary.Labels?["com.docker.compose.project"]
        )
    }
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
}

private struct Info: Decodable {
    var ServerVersion: String
    var NCPU: Int
    var MemTotal: Int64
}
