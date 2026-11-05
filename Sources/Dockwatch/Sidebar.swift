import AppKit

private final class SidebarCell: NSTableCellView {
    let detail = NSTextField(labelWithString: "")
}

final class SidebarController: NSViewController, NSTableViewDataSource, NSTableViewDelegate {
    private enum Row {
        case header(String)
        case runtime(Int)
        case section(Section)
    }

    weak var main: MainWindowController?
    private let tableView = NSTableView()
    private var rows: [Row] = []

    override func loadView() {
        let scroll = NSScrollView()
        scroll.documentView = tableView
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        tableView.style = .sourceList
        tableView.rowSizeStyle = .default
        tableView.headerView = nil
        tableView.addTableColumn(NSTableColumn(identifier: NSUserInterfaceItemIdentifier("main")))
        tableView.dataSource = self
        tableView.delegate = self
        view = scroll
    }

    func reload() {
        guard let main else { return }
        loadViewIfNeeded()
        rows = [.header("Runtimes")] + main.runtimes.indices.map(Row.runtime)
            + [.header("Resources")] + Section.allCases.map(Row.section)
        tableView.reloadData()
        if let index = rows.firstIndex(where: { if case .section(main.section) = $0 { true } else { false } }) {
            tableView.selectRowIndexes([index], byExtendingSelection: false)
        }
    }

    func numberOfRows(in tableView: NSTableView) -> Int {
        rows.count
    }

    func tableView(_ tableView: NSTableView, isGroupRow row: Int) -> Bool {
        if case .header = rows[row] { true } else { false }
    }

    /// A runtime row switches the connection and leaves the selection on the section.
    func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool {
        switch rows[row] {
        case .header: return false
        case let .runtime(index):
            main?.switchRuntime(index)
            return false
        case .section: return true
        }
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard
            let main, rows.indices.contains(tableView.selectedRow),
            case let .section(section) = rows[tableView.selectedRow], section != main.section
        else { return }
        main.showSection(section)
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard let main else { return nil }
        let cell = reusableCell(rows[row])
        switch rows[row] {
        case let .header(title):
            cell.textField?.stringValue = title
        case let .runtime(index):
            let runtime = main.runtimes[index]
            let chosen = runtime.socket == main.chosenSocket
            cell.textField?.stringValue = runtime.name
            cell.textField?.font = chosen ? .boldSystemFont(ofSize: NSFont.systemFontSize) : .systemFont(ofSize: NSFont.systemFontSize)
            cell.imageView?.image = NSImage(systemSymbolName: "circle.fill", accessibilityDescription: runtime.running ? "Running" : "Stopped")?
                .withSymbolConfiguration(.init(pointSize: 7, weight: .regular))
            cell.imageView?.contentTintColor = runtime.running ? .systemGreen : .tertiaryLabelColor
            cell.detail.stringValue = index < 9 ? "⌘\(index + 1)" : ""
            var tooltip = runtime.socket
            if chosen, let engine = main.engine {
                tooltip += "\nEngine \(engine.version), \(engine.cpus) CPUs, \(formatBytes(UInt64(max(0, engine.memory)))) RAM"
            }
            cell.toolTip = tooltip
        case let .section(section):
            cell.textField?.stringValue = section.title
            cell.imageView?.image = NSImage(systemSymbolName: section.symbol, accessibilityDescription: section.title)
            cell.imageView?.contentTintColor = nil
            let count = switch section {
            case .containers: main.snapshot.containers.count
            case .images: main.snapshot.images.count
            case .networks: main.snapshot.networks.count
            }
            cell.detail.stringValue = main.docker == nil ? "" : "\(count)"
            cell.toolTip = nil
        }
        return cell
    }

    private func reusableCell(_ row: Row) -> SidebarCell {
        let isHeader = if case .header = row { true } else { false }
        let id = NSUserInterfaceItemIdentifier(isHeader ? "header" : "item")
        if let cell = tableView.makeView(withIdentifier: id, owner: nil) as? SidebarCell {
            return cell
        }
        let cell = SidebarCell()
        cell.identifier = id
        cell.detail.textColor = .secondaryLabelColor
        cell.detail.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        layoutCell(cell, withImage: !isHeader, detail: isHeader ? nil : cell.detail)
        return cell
    }
}
