import AppKit
import UniformTypeIdentifiers

struct FileEntry: Equatable {
    var path: String
    var name: String
    var isFolder: Bool
    var size: Int64
    var modified: Date
    var mode: String
}

/// Lines of `stat -c '%F\t%s\t%Y\t%A\t%n'`; folders first, then by name as Finder sorts.
/// shortcut: a name with a newline or tab breaks its line, fine until someone browses such files.
func parseListing(_ text: String) -> [FileEntry] {
    var entries: [FileEntry] = []
    for line in text.split(separator: "\n") {
        let fields = line.split(separator: "\t", maxSplits: 4, omittingEmptySubsequences: false)
        guard fields.count == 5, let size = Int64(fields[1]), let seconds = TimeInterval(fields[2]) else { continue }
        let path = String(fields[4])
        entries.append(FileEntry(
            path: path, name: String(path.split(separator: "/").last ?? ""), isFolder: fields[0] == "directory",
            size: size, modified: Date(timeIntervalSince1970: seconds), mode: String(fields[3])
        ))
    }
    return entries.sorted {
        $0.isFolder != $1.isFolder ? $0.isFolder : $0.name.localizedStandardCompare($1.name) == .orderedAscending
    }
}

/// Splits an exec stream without a TTY: 8-byte frame headers (1 stdout, 2 stderr, big-endian size).
func demultiplex(_ stream: Data) -> (output: Data, errors: Data) {
    let bytes = [UInt8](stream)
    var output = Data()
    var errors = Data()
    var offset = 0
    while bytes.count - offset >= 8 {
        let size = bytes[offset + 4 ..< offset + 8].reduce(0) { $0 << 8 | Int($1) }
        let end = min(bytes.count, offset + 8 + size)
        if bytes[offset] == 2 {
            errors += bytes[offset + 8 ..< end]
        } else {
            output += bytes[offset + 8 ..< end]
        }
        offset = end
    }
    return (output, errors)
}

/// Contents of the first regular file in a tar archive, skipping PAX and GNU long-name headers.
func firstFileInTar(_ tar: Data) -> Data? {
    let bytes = [UInt8](tar)
    var offset = 0
    while offset + 512 <= bytes.count {
        let header = bytes[offset ..< offset + 512]
        if header.allSatisfy({ $0 == 0 }) { return nil }
        let sizeField = String(decoding: header.dropFirst(124).prefix(12), as: UTF8.self)
        guard let size = Int(sizeField.trimmingCharacters(in: CharacterSet(charactersIn: " \0")), radix: 8) else { return nil }
        let type = header[offset + 156]
        let start = offset + 512
        if type == UInt8(ascii: "0") || type == 0 {
            guard start + size <= bytes.count else { return nil }
            return Data(bytes[start ..< start + size])
        }
        offset = start + (size + 511) / 512 * 512
    }
    return nil
}

/// The parent of `/etc/apk`, `/etc`, and `/` for `/` itself.
func parentFolder(_ path: String) -> String {
    let parts = path.split(separator: "/").dropLast()
    return "/" + parts.joined(separator: "/")
}

/// One folder of a running container on the left, the selected text file on the right.
final class FilesController: NSViewController, NSTableViewDataSource, NSTableViewDelegate {
    weak var main: MainWindowController?
    private var docker: Docker?
    private var containerID: String?
    private var folder = "/"
    private var entries: [FileEntry] = []

    private let pathBar = NSStackView()
    let table = NSTableView()
    private let message = NSTextField(wrappingLabelWithString: "")
    private let previewTitle = NSTextField(labelWithString: "")
    private let previewInfo = NSTextField(labelWithString: "")
    private let previewScroll = NSTextView.scrollableTextView()
    private var previewText: NSTextView { previewScroll.documentView as! NSTextView }
    private let monospaced = NSFont.monospacedSystemFont(ofSize: 11, weight: .regular)
    private static let previewLimit: Int64 = 512 * 1024

    private var selected: FileEntry? {
        table.selectedRow >= 0 ? entries[table.selectedRow] : nil
    }

    override func loadView() {
        pathBar.spacing = 2
        pathBar.setAccessibilityLabel("Path")
        let upload = NSButton(title: "Upload…", target: self, action: #selector(uploadClicked))
        let save = NSButton(title: "Save to Mac…", target: self, action: #selector(saveClicked))
        upload.setContentHuggingPriority(.required, for: .horizontal)
        save.setContentHuggingPriority(.required, for: .horizontal)

        table.style = .inset
        table.usesAlternatingRowBackgroundColors = true
        table.columnAutoresizingStyle = .firstColumnOnlyAutoresizingStyle
        for (identifier, width) in [("Name", 260.0), ("Size", 80.0), ("Modified", 180.0)] {
            let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(identifier))
            column.title = identifier
            column.width = width
            table.addTableColumn(column)
        }
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.doubleAction = #selector(openSelected(_:))
        table.registerForDraggedTypes([.fileURL])
        table.setAccessibilityLabel("Files")
        let tableScroll = NSScrollView()
        tableScroll.documentView = table
        tableScroll.hasVerticalScroller = true

        message.textColor = .secondaryLabelColor
        message.alignment = .center
        message.lineBreakMode = .byWordWrapping

        previewTitle.font = .boldSystemFont(ofSize: 13)
        previewTitle.lineBreakMode = .byTruncatingMiddle
        previewInfo.textColor = .secondaryLabelColor
        previewInfo.font = monospaced
        previewText.isEditable = false
        previewText.font = monospaced
        previewText.textContainerInset = NSSize(width: 8, height: 8)
        let previewHeader = NSStackView(views: [previewTitle, previewInfo])
        previewHeader.edgeInsets = NSEdgeInsets(top: 6, left: 12, bottom: 6, right: 12)
        let preview = NSStackView(views: [previewHeader, previewScroll])
        preview.orientation = .vertical
        preview.alignment = .leading
        preview.spacing = 0
        previewScroll.widthAnchor.constraint(equalTo: preview.widthAnchor).isActive = true

        let split = NSSplitView()
        split.isVertical = true
        split.dividerStyle = .thin
        split.addArrangedSubview(tableScroll)
        split.addArrangedSubview(preview)
        split.setHoldingPriority(.defaultLow, forSubviewAt: 0)
        preview.widthAnchor.constraint(greaterThanOrEqualToConstant: 300).isActive = true
        tableScroll.widthAnchor.constraint(greaterThanOrEqualToConstant: 320).isActive = true

        let root = NSView()
        // message after split: inside the scroll view the clip view covers it.
        for subview in [pathBar, upload, save, split, message] {
            subview.translatesAutoresizingMaskIntoConstraints = false
            root.addSubview(subview)
        }
        NSLayoutConstraint.activate([
            save.topAnchor.constraint(equalTo: root.topAnchor, constant: 6),
            save.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -12),
            upload.centerYAnchor.constraint(equalTo: save.centerYAnchor),
            upload.trailingAnchor.constraint(equalTo: save.leadingAnchor, constant: -8),
            pathBar.centerYAnchor.constraint(equalTo: save.centerYAnchor),
            // The content view runs under the floating sidebar; the safe area starts beside it.
            pathBar.leadingAnchor.constraint(equalTo: root.safeAreaLayoutGuide.leadingAnchor, constant: 12),
            pathBar.trailingAnchor.constraint(lessThanOrEqualTo: upload.leadingAnchor, constant: -8),
            split.topAnchor.constraint(equalTo: save.bottomAnchor, constant: 6),
            split.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            split.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            split.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            message.leadingAnchor.constraint(equalTo: root.safeAreaLayoutGuide.leadingAnchor, constant: 20),
            message.trailingAnchor.constraint(equalTo: tableScroll.trailingAnchor, constant: -20),
            message.centerYAnchor.constraint(equalTo: tableScroll.centerYAnchor),
        ])
        view = root
        showPreview(nil)
    }

    func open(docker: Docker, containerID: String) {
        loadViewIfNeeded()
        self.docker = docker
        self.containerID = containerID
        folder = "/"
        entries = []
        table.reloadData()
    }

    func close() {
        docker = nil
        containerID = nil
        entries = []
        table.reloadData()
        showPreview(nil)
    }

    func reload() {
        show(folder: folder)
    }

    /// Keeps the selection on `name`, or on the same entry when the folder reloads.
    private func show(folder newFolder: String, selecting name: String? = nil) {
        guard let docker, let id = containerID else { return }
        Task {
            do {
                let listed = try await docker.listFiles(id, in: newFolder)
                guard containerID == id else { return }
                let previous = name ?? (folder == newFolder ? selected?.name : nil)
                folder = newFolder
                entries = listed
                message.stringValue = listed.isEmpty ? "Empty folder" : ""
                table.reloadData()
                let row = entries.firstIndex { $0.name == previous } ?? 0
                if !entries.isEmpty { table.selectRowIndexes([row], byExtendingSelection: false) }
                table.scrollRowToVisible(row)
            } catch {
                guard containerID == id else { return }
                folder = newFolder
                entries = []
                table.reloadData()
                message.stringValue = error.localizedDescription
            }
            updatePath()
            showPreview(selected)
        }
    }

    /// One button per folder from `/` down; the button's tag is how many components it keeps.
    private func updatePath() {
        pathBar.arrangedSubviews.forEach { $0.removeFromSuperview() }
        let parts = folder.split(separator: "/").map(String.init)
        for (depth, title) in (["/"] + parts).enumerated() {
            if depth > 0 {
                let chevron = NSImageView(image: NSImage(systemSymbolName: "chevron.compact.right", accessibilityDescription: nil)!)
                chevron.contentTintColor = .tertiaryLabelColor
                pathBar.addArrangedSubview(chevron)
            }
            let button = NSButton(title: title, target: self, action: #selector(pathClicked))
            button.isBordered = false
            button.tag = depth
            button.contentTintColor = depth == parts.count ? .labelColor : .secondaryLabelColor
            if depth == 0 {
                button.image = NSImage(systemSymbolName: "folder", accessibilityDescription: nil)
                button.imagePosition = .imageLeading
            }
            pathBar.addArrangedSubview(button)
        }
    }

    @objc private func pathClicked(_ sender: NSButton) {
        show(folder: "/" + folder.split(separator: "/").prefix(sender.tag).joined(separator: "/"))
    }

    @objc func openSelected(_ sender: Any?) {
        guard let entry = selected, entry.isFolder else { return }
        show(folder: entry.path)
    }

    @objc func openEnclosingFolder(_ sender: Any?) {
        guard folder != "/" else { return }
        show(folder: parentFolder(folder), selecting: folder.split(separator: "/").last.map(String.init))
    }

    /// Return opens a folder; Cmd+Up and Cmd+Down come through the View menu.
    override func keyDown(with event: NSEvent) {
        if event.keyCode == 36 || event.keyCode == 76 {
            openSelected(nil)
        } else {
            super.keyDown(with: event)
        }
    }

    // MARK: Preview

    func tableViewSelectionDidChange(_ notification: Notification) {
        showPreview(selected)
    }

    private func showPreview(_ entry: FileEntry?) {
        previewText.string = ""
        guard let entry else {
            previewTitle.stringValue = ""
            previewInfo.stringValue = ""
            return
        }
        previewTitle.stringValue = entry.name
        previewInfo.stringValue = entry.isFolder ? entry.mode : "\(formatBytes(UInt64(entry.size))) · \(entry.mode)"
        guard !entry.isFolder else { return }
        guard entry.size <= Self.previewLimit else {
            previewText.string = "Too large to preview. Save it to your Mac to open it."
            return
        }
        guard let docker, let id = containerID else { return }
        Task {
            let data = try? await docker.archive(id, path: entry.path)
            guard selected == entry else { return }
            previewText.string = data.flatMap(firstFileInTar).map(previewString) ?? "No preview."
        }
    }

    private func previewString(_ data: Data) -> String {
        guard !data.contains(0), let text = String(data: data, encoding: .utf8) else { return "Binary file, no preview." }
        return text
    }

    // MARK: Save and upload

    @objc private func saveClicked() {
        guard let entry = selected, let window = view.window, let docker, let id = containerID else { return }
        let panel = NSSavePanel()
        panel.nameFieldStringValue = entry.name
        panel.beginSheetModal(for: window) { [weak self] response in
            guard response == .OK, let destination = panel.url else { return }
            Task {
                do {
                    try await saveToMac(docker: docker, containerID: id, entry: entry, destination: destination)
                } catch {
                    self?.main?.showError(error)
                }
            }
        }
    }

    @objc private func uploadClicked() {
        guard let window = view.window else { return }
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = true
        panel.prompt = "Upload"
        panel.message = "Copy into \(folder)"
        panel.beginSheetModal(for: window) { [weak self] response in
            guard response == .OK else { return }
            self?.upload(panel.urls)
        }
    }

    private func upload(_ urls: [URL]) {
        guard let docker, let id = containerID else { return }
        let target = folder
        Task {
            do {
                for url in urls {
                    try await uploadFromMac(docker: docker, containerID: id, source: url, folder: target)
                }
            } catch {
                main?.showError(error)
            }
            if containerID == id, folder == target { reload() }
        }
    }

    func tableView(
        _ tableView: NSTableView, validateDrop info: NSDraggingInfo, proposedRow row: Int,
        proposedDropOperation dropOperation: NSTableView.DropOperation
    ) -> NSDragOperation {
        guard containerID != nil else { return [] }
        tableView.setDropRow(-1, dropOperation: .on)
        return .copy
    }

    func tableView(
        _ tableView: NSTableView, acceptDrop info: NSDraggingInfo, row: Int, dropOperation: NSTableView.DropOperation
    ) -> Bool {
        let urls = info.draggingPasteboard.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL] ?? []
        guard !urls.isEmpty else { return false }
        upload(urls)
        return true
    }

    // MARK: Table

    func numberOfRows(in tableView: NSTableView) -> Int {
        entries.count
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard let column = tableColumn?.identifier else { return nil }
        let isName = column.rawValue == "Name"
        let cell = tableView.makeView(withIdentifier: column, owner: nil) as? NSTableCellView ?? {
            let cell = NSTableCellView()
            cell.identifier = column
            layoutCell(cell, withImage: isName)
            if !isName { cell.textField?.textColor = .secondaryLabelColor }
            return cell
        }()
        let entry = entries[row]
        switch column.rawValue {
        case "Name":
            cell.textField?.stringValue = entry.name
            cell.imageView?.image = entry.isFolder
                ? NSWorkspace.shared.icon(for: .folder)
                : NSWorkspace.shared.icon(for: UTType(filenameExtension: (entry.name as NSString).pathExtension) ?? .data)
        case "Size":
            cell.textField?.stringValue = entry.isFolder ? "--" : formatBytes(UInt64(entry.size))
        default:
            cell.textField?.stringValue = entry.modified.formatted(date: .abbreviated, time: .shortened)
        }
        return cell
    }
}


/// Docker sends a tar; the system `tar` unpacks it, so folders keep their tree, links and modes.
private func saveToMac(docker: Docker, containerID: String, entry: FileEntry, destination: URL) async throws {
    let scratch = FileManager.default.temporaryDirectory.appendingPathComponent("dockwatch-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: scratch, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: scratch) }
    let archive = scratch.appendingPathComponent("download.tar")
    try await docker.downloadArchive(containerID, path: entry.path, to: archive)
    try runTar(["-xf", archive.path, "-C", scratch.path])
    // The save panel already confirmed replacing an existing item.
    try? FileManager.default.removeItem(at: destination)
    try FileManager.default.moveItem(at: scratch.appendingPathComponent(entry.name), to: destination)
}

private func uploadFromMac(docker: Docker, containerID: String, source: URL, folder: String) async throws {
    let scratch = FileManager.default.temporaryDirectory.appendingPathComponent("dockwatch-\(UUID().uuidString).tar")
    defer { try? FileManager.default.removeItem(at: scratch) }
    try makeUploadArchive(of: source, at: scratch)
    try await docker.uploadArchive(containerID, tar: try Data(contentsOf: scratch, options: .mappedIfSafe), into: folder)
}

// Docker rejects macOS xattrs such as com.apple.macl ("lsetxattr: operation not supported"),
// and --no-mac-metadata alone still adds `._name` AppleDouble files.
func makeUploadArchive(of source: URL, at archive: URL) throws {
    try runTar(["-cf", archive.path, "--no-mac-metadata", "--no-xattrs", "--no-acls", "--no-fflags",
                "-C", source.deletingLastPathComponent().path, source.lastPathComponent])
}

private func runTar(_ arguments: [String]) throws {
    let process = Process()
    process.executableURL = URL(fileURLWithPath: "/usr/bin/tar")
    process.arguments = arguments
    let errors = Pipe()
    process.standardError = errors
    try process.run()
    let message = errors.fileHandleForReading.readDataToEndOfFile()
    process.waitUntilExit()
    guard process.terminationStatus == 0 else {
        throw DockerError(message: "tar: " + String(decoding: message, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines))
    }
}
