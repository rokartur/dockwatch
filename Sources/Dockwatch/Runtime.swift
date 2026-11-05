import Foundation

struct Runtime: Equatable {
    var name: String
    var socket: String
    var running = false
    var current = false
}

private let knownSockets: [(name: String, path: String)] = [
    ("Docker Desktop", "~/.docker/run/docker.sock"),
    ("OrbStack", "~/.orbstack/run/docker.sock"),
    ("Lighter", "~/.lighter/docker.sock"),
    ("Colima", "~/.colima/default/docker.sock"),
    ("Colima", "~/.config/colima/default/docker.sock"),
    ("Rancher Desktop", "~/.rd/docker.sock"),
    ("Lima", "~/.lima/docker/sock/docker.sock"),
    ("Docker", "/var/run/docker.sock"),
]

/// Runtimes from `DOCKER_HOST`, every Docker context and the known socket paths, each pinged.
func discoverRuntimes() async -> [Runtime] {
    let home = FileManager.default.homeDirectoryForCurrentUser.path
    let dockerHost = ProcessInfo.processInfo.environment["DOCKER_HOST"]
    let (contexts, currentContext) = readContexts(home: home)
    var runtimes = runtimeCandidates(
        home: home, dockerHost: dockerHost, contexts: contexts, currentContext: currentContext,
        exists: { FileManager.default.fileExists(atPath: $0) }
    )
    await withTaskGroup(of: (Int, Bool).self) { group in
        for (index, runtime) in runtimes.enumerated() {
            group.addTask { (index, await Docker(socket: runtime.socket).ping()) }
        }
        for await (index, running) in group {
            runtimes[index].running = running
        }
    }
    return runtimes
}

/// The preferred socket when it runs, else the current Docker context, else the first running runtime.
func launchRuntime(_ runtimes: [Runtime], preferred: String?) -> Runtime? {
    runtimes.first { $0.socket == preferred && $0.running }
        ?? runtimes.first { $0.current && $0.running }
        ?? runtimes.first { $0.running }
}

/// Context name and endpoint host from `~/.docker/contexts/meta/*/meta.json`, plus `currentContext`.
private func readContexts(home: String) -> (contexts: [(name: String, host: String)], current: String?) {
    let dockerDir = URL(fileURLWithPath: home).appendingPathComponent(".docker")
    let metaDir = dockerDir.appendingPathComponent("contexts/meta")
    var contexts: [(name: String, host: String)] = []
    let entries = (try? FileManager.default.contentsOfDirectory(at: metaDir, includingPropertiesForKeys: nil)) ?? []
    for entry in entries {
        guard
            let meta = readJSON(entry.appendingPathComponent("meta.json")),
            let name = meta["Name"] as? String,
            let endpoints = meta["Endpoints"] as? [String: Any],
            let docker = endpoints["docker"] as? [String: Any],
            let host = docker["Host"] as? String
        else { continue }
        contexts.append((name, host))
    }
    let current = readJSON(dockerDir.appendingPathComponent("config.json"))?["currentContext"] as? String
    return (contexts, current)
}

private func readJSON(_ url: URL) -> [String: Any]? {
    guard let data = try? Data(contentsOf: url) else { return nil }
    return (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
}

/// Ordered and deduplicated by real socket path: `DOCKER_HOST`, contexts by name, then known paths
/// that exist. The `default` context is skipped, it only repeats `DOCKER_HOST` or `/var/run/docker.sock`.
/// Remote hosts (`ssh://`, `tcp://`) are skipped too.
func runtimeCandidates(
    home: String,
    dockerHost: String?,
    contexts: [(name: String, host: String)],
    currentContext: String?,
    exists: (String) -> Bool
) -> [Runtime] {
    let known = knownSockets.map { (name: $0.name, path: expandHome($0.path, home: home)) }
    func knownName(_ socket: String) -> String? {
        known.first { realPath($0.path) == realPath(socket) }?.name
    }

    var found: [Runtime] = []
    if let socket = dockerHost.flatMap(unixSocket) {
        found.append(Runtime(name: knownName(socket) ?? "DOCKER_HOST", socket: socket, current: true))
    }
    for context in contexts.filter({ $0.name != "default" }).sorted(by: { $0.name < $1.name }) {
        guard let socket = unixSocket(context.host) else { continue }
        let current = dockerHost == nil && currentContext == context.name
        found.append(Runtime(name: knownName(socket) ?? context.name, socket: socket, current: current))
    }
    for socket in known where exists(socket.path) {
        found.append(Runtime(name: socket.name, socket: socket.path))
    }

    var runtimes: [Runtime] = []
    for runtime in found {
        if let seen = runtimes.firstIndex(where: { realPath($0.socket) == realPath(runtime.socket) }) {
            runtimes[seen].current = runtimes[seen].current || runtime.current
            continue
        }
        runtimes.append(runtime)
    }
    return runtimes
}

private func unixSocket(_ host: String) -> String? {
    host.hasPrefix("unix://") ? String(host.dropFirst("unix://".count)) : nil
}

private func expandHome(_ path: String, home: String) -> String {
    path.hasPrefix("~/") ? home + path.dropFirst(1) : path
}

private func realPath(_ path: String) -> String {
    URL(fileURLWithPath: path).resolvingSymlinksInPath().path
}
