import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// Path of the app chosen in Settings; empty means the system default for `.command` files.
let terminalAppKey = "terminalApp"

/// Shell script that opens an interactive shell in the container through the docker CLI.
func terminalScript(socket: String, containerID: String) -> String {
    """
    #!/bin/sh
    rm -f "$0"
    PATH="$PATH:/opt/homebrew/bin:/usr/local/bin:$HOME/.orbstack/bin:/Applications/Docker.app/Contents/Resources/bin"
    unset DOCKER_CONTEXT
    export DOCKER_HOST=\(shellQuote("unix://" + socket))
    exec docker exec -it \(shellQuote(containerID)) sh -c 'command -v bash >/dev/null && exec bash || exec sh'

    """
}

func shellQuote(_ value: String) -> String {
    "'" + value.replacingOccurrences(of: "'", with: #"'\''"#) + "'"
}

/// Terminal, iTerm and Ghostty all run a `.command` file they are asked to open.
func openTerminal(socket: String, containerID: String) throws {
    let script = FileManager.default.temporaryDirectory.appendingPathComponent("dockwatch-\(UUID().uuidString).command")
    try terminalScript(socket: socket, containerID: containerID).write(to: script, atomically: true, encoding: .utf8)
    try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: script.path)
    let chosen = UserDefaults.standard.string(forKey: terminalAppKey) ?? ""
    guard let app = chosen.isEmpty ? NSWorkspace.shared.urlForApplication(toOpen: script) : URL(fileURLWithPath: chosen) else {
        throw DockerError(message: "No app opens .command files. Choose a terminal in Settings.")
    }
    NSWorkspace.shared.open([script], withApplicationAt: app, configuration: NSWorkspace.OpenConfiguration())
}

struct SettingsView: View {
    @AppStorage(terminalAppKey) private var terminalApp = ""
    private let apps = NSWorkspace.shared.urlsForApplications(toOpen: UTType(filenameExtension: "command") ?? .shellScript)

    var body: some View {
        Form {
            Picker("Open terminals in", selection: $terminalApp) {
                Text("Default app for .command files").tag("")
                ForEach(apps, id: \.path) { app in
                    Text(app.deletingPathExtension().lastPathComponent).tag(app.path)
                }
            }
        }
        .formStyle(.grouped)
        .frame(width: 460)
        .fixedSize()
    }
}
