import AppKit
import SwiftUI

/// Logs, Stats, Files, Inspect and Env of one container, or Inspect alone for an image or network.
final class DetailController: NSViewController, NSTableViewDataSource, NSTableViewDelegate {
    weak var main: MainWindowController? {
        didSet { files.main = main }
    }
    private(set) var pane = Pane.logs
    private var docker: Docker?
    private var openID: String?
    private var containerID: String?
    private var logStream: DockerStream?
    private var statsStream: DockerStream?
    private var decoder = LogDecoder()
    private var statsLines = LineSplitter()
    private var buffer = LogBuffer()
    private var pendingLines: [LogLine] = []
    private var flushScheduled = false
    private var filter = ""
    private var showTimestamps = true
    private var history = StatsHistory()
    private var env: [(key: String, value: String)] = []

    private let logScroll = NSTextView.scrollableTextView()
    private let inspectScroll = NSTextView.scrollableTextView()
    private lazy var statsView = NSHostingView(rootView: StatsView(samples: []))
    private let files = FilesController()
    private let envScroll = NSScrollView()
    private let envTable = NSTableView()
    private let monospaced = NSFont.monospacedSystemFont(ofSize: 11, weight: .regular)

    private var logText: NSTextView { logScroll.documentView as! NSTextView }
    private var inspectText: NSTextView { inspectScroll.documentView as! NSTextView }
    private var paneViews: [NSView] { [logScroll, statsView, files.view, inspectScroll, envScroll] }

    /// Gets focus on open, so Cmd+F and Cmd+A act on the text.
    var focusView: NSView {
        if containerID == nil || pane == .inspect { return inspectText }
        return pane == .files ? files.table : logText
    }

    override func loadView() {
        let container = NSView()
        addChild(files)
        for text in [logText, inspectText] {
            text.isEditable = false
            text.isRichText = false
            text.font = monospaced
            text.usesFindBar = true
            text.isIncrementalSearchingEnabled = true
            text.textContainerInset = NSSize(width: 12, height: 8)
        }
        inspectScroll.hasHorizontalScroller = true
        inspectText.isHorizontallyResizable = true
        inspectText.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        inspectText.textContainer?.widthTracksTextView = false
        inspectText.textContainer?.containerSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)

        envTable.style = .inset
        envTable.usesAlternatingRowBackgroundColors = true
        envTable.columnAutoresizingStyle = .lastColumnOnlyAutoresizingStyle
        for (identifier, width) in [("Key", 240.0), ("Value", 500.0)] {
            let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(identifier))
            column.title = identifier
            column.width = width
            envTable.addTableColumn(column)
        }
        envTable.dataSource = self
        envTable.delegate = self
        envScroll.documentView = envTable
        envScroll.hasVerticalScroller = true

        for pane in paneViews {
            pane.translatesAutoresizingMaskIntoConstraints = false
            container.addSubview(pane)
            // Text panes stop at the toolbar: lines scrolling under the window title are unreadable.
            let isTextPane = pane === logScroll || pane === inspectScroll || pane === files.view
            let top = isTextPane ? container.safeAreaLayoutGuide.topAnchor : container.topAnchor
            NSLayoutConstraint.activate([
                pane.topAnchor.constraint(equalTo: top),
                pane.bottomAnchor.constraint(equalTo: container.bottomAnchor),
                pane.leadingAnchor.constraint(equalTo: container.leadingAnchor),
                pane.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            ])
        }
        view = container
        show(pane)
    }

    func open(_ section: Section, id: String, docker: Docker) {
        loadViewIfNeeded()
        close()
        self.docker = docker
        openID = id
        loadInspect(section, id)
        if section == .containers {
            containerID = id
            files.open(docker: docker, containerID: id)
            startStreams()
            show(pane)
        } else {
            setVisiblePane(.inspect)
        }
    }

    func close() {
        logStream?.cancel()
        statsStream?.cancel()
        logStream = nil
        statsStream = nil
        docker = nil
        openID = nil
        containerID = nil
        files.close()
        filter = ""
        resetLogs()
        history.clear()
        renderStatsIfVisible()
        inspectText.string = ""
        env = []
        envTable.reloadData()
    }

    /// A stopped container's log stream ends; once it runs again, both streams start over.
    func reopenStreams() {
        logStream?.cancel()
        statsStream?.cancel()
        resetLogs()
        history.clear()
        renderStatsIfVisible()
        startStreams()
    }

    func show(_ newPane: Pane) {
        pane = newPane
        setVisiblePane(newPane)
        renderStatsIfVisible()
        if newPane == .files {
            files.reload()
            view.window?.makeFirstResponder(files.table)
        }
    }

    /// A hidden NSHostingView still lays out its charts, so samples render only while Stats shows.
    private func renderStatsIfVisible() {
        guard pane == .stats else { return }
        statsView.rootView = StatsView(samples: history.samples)
    }

    private func setVisiblePane(_ visible: Pane) {
        for (index, view) in paneViews.enumerated() {
            view.isHidden = index != visible.rawValue
        }
    }

    func setFilter(_ query: String) {
        filter = query
        renderLogs()
    }

    func toggleTimestamps() {
        showTimestamps.toggle()
        renderLogs()
    }

    private func resetLogs() {
        decoder = LogDecoder()
        statsLines = LineSplitter()
        buffer.clear()
        pendingLines = []
        logText.string = ""
    }

    private func startStreams() {
        guard let docker, let id = containerID else { return }
        do {
            // A stream ends when the container stops or goes away; the snapshot handles both.
            logStream = try docker.logs(id, onData: { [weak self] chunk, contentType in
                self?.receiveLogs(chunk, contentType)
            }, onEnd: { _ in })
            statsStream = try docker.stats(id, onData: { [weak self] chunk, _ in
                self?.receiveStats(chunk)
            }, onEnd: { _ in })
        } catch {
            main?.showError(error)
        }
    }

    private func receiveStats(_ chunk: Data) {
        for line in statsLines.feed(chunk) {
            if let stats = try? JSONDecoder().decode(StatsResponse.self, from: Data(line.utf8)) {
                history.push(Sample(stats))
                renderStatsIfVisible()
            }
        }
    }

    /// Lines collect for 100 ms, so a chatty container costs one text update per batch, not per line.
    private func receiveLogs(_ chunk: Data, _ contentType: String) {
        pendingLines += decoder.feed(chunk, contentType: contentType)
        guard !flushScheduled else { return }
        flushScheduled = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) { [weak self] in
            self?.flushLogs()
        }
    }

    private func flushLogs() {
        flushScheduled = false
        let new = pendingLines
        pendingLines = []
        let before = buffer.lines.count
        let dropped = buffer.append(new)
        if dropped.count > before {
            renderLogs()
            return
        }
        let follow = isScrolledToBottom
        let storage = logText.textStorage!
        storage.beginEditing()
        let droppedShown = dropped.filter { logLineMatches($0, filter) }.count
        if droppedShown > 0 {
            let string = storage.mutableString
            var end = 0
            for _ in 0..<droppedShown {
                end = NSMaxRange(string.lineRange(for: NSRange(location: end, length: 0)))
            }
            storage.deleteCharacters(in: NSRange(location: 0, length: end))
        }
        storage.append(display(new))
        storage.endEditing()
        if follow {
            logText.scrollToEndOfDocument(nil)
        }
    }

    private func renderLogs() {
        logText.textStorage?.setAttributedString(display(buffer.lines[...]))
        logText.scrollToEndOfDocument(nil)
    }

    private func display(_ lines: some Sequence<LogLine>) -> NSAttributedString {
        let text = NSMutableAttributedString()
        let timeAttributes: [NSAttributedString.Key: Any] = [.font: monospaced, .foregroundColor: NSColor.secondaryLabelColor]
        let textAttributes: [NSAttributedString.Key: Any] = [.font: monospaced, .foregroundColor: NSColor.textColor]
        for line in lines where logLineMatches(line, filter) {
            if showTimestamps {
                text.append(NSAttributedString(string: line.time + " ", attributes: timeAttributes))
            }
            text.append(NSAttributedString(string: line.text + "\n", attributes: textAttributes))
        }
        return text
    }

    private var isScrolledToBottom: Bool {
        logScroll.contentView.bounds.maxY >= logText.frame.height - 24
    }

    private func loadInspect(_ section: Section, _ id: String) {
        guard let docker else { return }
        Task {
            do {
                let object = try await docker.inspect(section, id)
                guard openID == id else { return }
                let json = try JSONSerialization.data(withJSONObject: object, options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes])
                inspectText.textStorage?.setAttributedString(highlightJSON(String(decoding: json, as: UTF8.self), font: monospaced))
                env = environment(object)
                envTable.reloadData()
            } catch {
                guard openID == id else { return }
                main?.showError(error)
            }
        }
    }

    private func environment(_ inspect: Any) -> [(key: String, value: String)] {
        let config = (inspect as? [String: Any])?["Config"] as? [String: Any]
        let entries = config?["Env"] as? [String] ?? []
        return entries.map { entry in
            let parts = entry.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
            return (String(parts[0]), parts.count > 1 ? String(parts[1]) : "")
        }
    }

    func numberOfRows(in tableView: NSTableView) -> Int {
        env.count
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard let column = tableColumn?.identifier else { return nil }
        let cell = tableView.makeView(withIdentifier: column, owner: nil) as? NSTableCellView ?? {
            let cell = NSTableCellView()
            cell.identifier = column
            layoutCell(cell, withImage: false)
            cell.textField?.isSelectable = true
            cell.textField?.font = monospaced
            return cell
        }()
        cell.textField?.stringValue = column.rawValue == "Key" ? env[row].key : env[row].value
        return cell
    }
}

private let jsonToken = try! NSRegularExpression(pattern: #""(?:[^"\\]|\\.)*"(\s*:)?|\b(?:true|false|null)\b|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?"#)

/// Xcode-like coloring of pretty-printed JSON: keys, strings, keywords, numbers.
func highlightJSON(_ json: String, font: NSFont) -> NSAttributedString {
    let result = NSMutableAttributedString(string: json, attributes: [.font: font, .foregroundColor: NSColor.labelColor])
    let text = json as NSString
    for match in jsonToken.matches(in: json, range: NSRange(location: 0, length: text.length)) {
        var range = match.range
        let colon = match.range(at: 1)
        let first = text.substring(with: NSRange(location: range.location, length: 1))
        let color: NSColor
        if colon.location != NSNotFound {
            range.length -= colon.length
            color = .systemBlue
        } else if first == "\"" {
            color = .systemOrange
        } else if first == "t" || first == "f" || first == "n" {
            color = .systemPink
        } else {
            color = .systemPurple
        }
        result.addAttribute(.foregroundColor, value: color, range: range)
    }
    return result
}
