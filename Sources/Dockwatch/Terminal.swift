import AppKit

/// Path of the app that opens terminals; empty means the system default for `.command` files.
let terminalAppKey = "terminalApp"
/// Path of the docker CLI for terminals; empty means `docker` on the script's PATH.
let dockerPathKey = "dockerPath"

/// Shell script that opens an interactive shell in the container through the docker CLI.
/// An empty `dockerPath` runs `docker` from PATH.
func terminalScript(socket: String, containerID: String, dockerPath: String) -> String {
    let docker = dockerPath.isEmpty ? "docker" : shellQuote(dockerPath)
    return """
    #!/bin/sh
    rm -f "$0"
    PATH="$PATH:/opt/homebrew/bin:/usr/local/bin:$HOME/.orbstack/bin:/Applications/Docker.app/Contents/Resources/bin"
    unset DOCKER_CONTEXT
    export DOCKER_HOST=\(shellQuote("unix://" + socket))
    exec \(docker) exec -it \(shellQuote(containerID)) sh -c 'command -v bash >/dev/null && exec bash || exec sh'

    """
}

func shellQuote(_ value: String) -> String {
    "'" + value.replacingOccurrences(of: "'", with: #"'\''"#) + "'"
}

/// Terminal, iTerm and Ghostty all run a `.command` file they are asked to open.
func openTerminal(socket: String, containerID: String) throws {
    let script = FileManager.default.temporaryDirectory.appendingPathComponent("dockwatch-\(UUID().uuidString).command")
    let dockerPath = NSString(string: UserDefaults.standard.string(forKey: dockerPathKey) ?? "").expandingTildeInPath
    try terminalScript(socket: socket, containerID: containerID, dockerPath: dockerPath).write(to: script, atomically: true, encoding: .utf8)
    try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: script.path)
    let chosen = UserDefaults.standard.string(forKey: terminalAppKey) ?? ""
    guard let app = chosen.isEmpty ? NSWorkspace.shared.urlForApplication(toOpen: script) : URL(fileURLWithPath: chosen) else {
        throw DockerError(message: "No app opens .command files. Choose one in the Open in Terminal menu.")
    }
    NSWorkspace.shared.open([script], withApplicationAt: app, configuration: NSWorkspace.OpenConfiguration())
}

/// "Open with" app choice and docker CLI path, rebuilt on every open so the checkmark follows the defaults.
final class TerminalMenu: NSObject, NSMenuDelegate {
    let menu = NSMenu(title: "Terminal")

    override init() {
        super.init()
        menu.delegate = self
    }

    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        menu.addItem(.sectionHeader(title: "Open With"))
        let chosen = UserDefaults.standard.string(forKey: terminalAppKey) ?? ""
        // Every text editor claims .command files too; these terminals run them.
        let apps = ["com.apple.Terminal", "com.googlecode.iterm2", "com.mitchellh.ghostty"]
            .compactMap(NSWorkspace.shared.urlForApplication(withBundleIdentifier:))
        let choices = [("Default App", "")] + apps.map { ($0.deletingPathExtension().lastPathComponent, $0.path) }
        for (title, path) in choices {
            let item = NSMenuItem(title: title, action: #selector(chooseApp), keyEquivalent: "")
            item.target = self
            item.representedObject = path
            item.state = path == chosen ? .on : .off
            menu.addItem(item)
        }
        menu.addItem(.separator())
        let path = UserDefaults.standard.string(forKey: dockerPathKey) ?? ""
        let docker = NSMenuItem(title: "docker CLI: \(path.isEmpty ? "docker on PATH" : path)…", action: #selector(chooseDockerPath), keyEquivalent: "")
        docker.target = self
        menu.addItem(docker)
    }

    @objc private func chooseApp(_ sender: NSMenuItem) {
        UserDefaults.standard.set(sender.representedObject as? String, forKey: terminalAppKey)
    }

    @objc private func chooseDockerPath(_ sender: Any?) {
        let alert = NSAlert()
        alert.messageText = "docker CLI for terminals"
        alert.informativeText = "Leave empty to run docker from PATH."
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 300, height: 22))
        field.stringValue = UserDefaults.standard.string(forKey: dockerPathKey) ?? ""
        field.placeholderString = "/opt/homebrew/bin/docker"
        alert.accessoryView = field
        alert.window.initialFirstResponder = field
        alert.addButton(withTitle: "Save")
        alert.addButton(withTitle: "Cancel")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        UserDefaults.standard.set(field.stringValue.trimmingCharacters(in: .whitespaces), forKey: dockerPathKey)
    }
}
