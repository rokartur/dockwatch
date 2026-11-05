import AppKit

enum ListRow: Equatable {
    case group(name: String, running: Int, total: Int)
    case container(Container)
    case image(Image, inUse: Bool)
    case network(Network, containers: Int)

    /// What actions and the detail act on; group headers have none.
    var id: String? {
        switch self {
        case .group: nil
        case let .container(container): container.id
        case let .image(image, _): image.reference
        case let .network(network, _): network.id
        }
    }
}

func listRows(_ snapshot: Snapshot, section: Section, onlyRunning: Bool, search: String) -> [ListRow] {
    func matches(_ fields: String...) -> Bool {
        search.isEmpty || fields.contains { $0.range(of: search, options: .caseInsensitive) != nil }
    }
    switch section {
    case .containers:
        let containers = snapshot.containers.filter {
            (!onlyRunning || $0.running) && matches($0.name, $0.image, $0.project ?? "")
        }
        return groupedByProject(containers)
    case .images:
        let used = Set(snapshot.containers.map(\.imageID))
        return snapshot.images
            .filter { matches($0.reference) }
            .sorted { $0.reference < $1.reference }
            .map { .image($0, inUse: used.contains($0.id)) }
    case .networks:
        var rows: [ListRow] = []
        for network in snapshot.networks.sorted(by: { $0.name < $1.name }) where matches(network.name, network.driver) {
            let attached = snapshot.containers.filter { $0.networks.contains(network.name) }.count
            rows.append(.network(network, containers: attached))
        }
        return rows
    }
}

/// Compose projects by name with a header each, containers without one last under "Other".
/// Without any compose project there are no headers.
func groupedByProject(_ containers: [Container]) -> [ListRow] {
    let sorted = containers.sorted { $0.name < $1.name }
    guard sorted.contains(where: { $0.project != nil }) else { return sorted.map(ListRow.container) }
    let groups = Dictionary(grouping: sorted) { $0.project ?? "" }
    let names = groups.keys.filter { !$0.isEmpty }.sorted() + (groups[""] == nil ? [] : [""])
    var rows: [ListRow] = []
    for name in names {
        let members = groups[name] ?? []
        rows.append(.group(name: name.isEmpty ? "Other" : name, running: members.filter(\.running).count, total: members.count))
        rows += members.map(ListRow.container)
    }
    return rows
}

final class ListController: NSViewController, NSTableViewDataSource, NSTableViewDelegate {
    weak var main: MainWindowController?
    let tableView = NSTableView()
    private let placeholder = NSTextField(labelWithString: "")
    private var rows: [ListRow] = []
    private var columnsSection: Section?
    private static let relativeDate = RelativeDateTimeFormatter()

    override func loadView() {
        let scroll = NSScrollView()
        scroll.documentView = tableView
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        tableView.style = .inset
        tableView.usesAlternatingRowBackgroundColors = true
        tableView.rowHeight = 22
        tableView.columnAutoresizingStyle = .firstColumnOnlyAutoresizingStyle
        tableView.dataSource = self
        tableView.delegate = self
        tableView.target = self
        tableView.doubleAction = #selector(openClicked)

        placeholder.font = .preferredFont(forTextStyle: .title3)
        placeholder.textColor = .secondaryLabelColor
        placeholder.isHidden = true

        let container = NSView()
        for subview in [scroll, placeholder] {
            subview.translatesAutoresizingMaskIntoConstraints = false
            container.addSubview(subview)
        }
        NSLayoutConstraint.activate([
            scroll.topAnchor.constraint(equalTo: container.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: container.bottomAnchor),
            scroll.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            placeholder.centerXAnchor.constraint(equalTo: container.centerXAnchor),
            placeholder.centerYAnchor.constraint(equalTo: container.centerYAnchor),
        ])
        view = container
    }

    func reload() {
        guard let main else { return }
        loadViewIfNeeded()
        if columnsSection != main.section {
            setColumns(main.section)
        }
        let selected = selectedID
        rows = listRows(main.snapshot, section: main.section, onlyRunning: main.onlyRunning, search: main.search)
        tableView.reloadData()
        if let selected, let index = rows.firstIndex(where: { $0.id == selected }) {
            tableView.selectRowIndexes([index], byExtendingSelection: false)
        }
        placeholder.isHidden = !rows.isEmpty
        if main.docker == nil {
            placeholder.stringValue = "No running Docker runtime"
        } else if !main.search.isEmpty || main.onlyRunning {
            placeholder.stringValue = "No matches"
        } else {
            placeholder.stringValue = "No \(main.section.title.lowercased())"
        }
    }

    var selectedID: String? {
        rows.indices.contains(tableView.selectedRow) ? rows[tableView.selectedRow].id : nil
    }

    @objc private func openClicked() {
        guard rows.indices.contains(tableView.clickedRow), let id = rows[tableView.clickedRow].id else { return }
        main?.open(id)
    }

    private func setColumns(_ section: Section) {
        columnsSection = section
        for column in tableView.tableColumns {
            tableView.removeTableColumn(column)
        }
        let columns: [(String, String, CGFloat)] = switch section {
        case .containers:
            [("name", "Name", 260), ("image", "Image", 240), ("ports", "Ports", 150), ("status", "Status", 200)]
        case .images:
            [("name", "Name", 320), ("tag", "Tag", 140), ("id", "Image ID", 120), ("created", "Created", 140), ("size", "Size", 90)]
        case .networks:
            [("name", "Name", 280), ("driver", "Driver", 100), ("scope", "Scope", 80), ("containers", "Containers", 90), ("id", "Network ID", 120)]
        }
        for (identifier, title, width) in columns {
            let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(identifier))
            column.title = title
            column.width = width
            column.minWidth = 60
            tableView.addTableColumn(column)
        }
    }

    func numberOfRows(in tableView: NSTableView) -> Int {
        rows.count
    }

    func tableView(_ tableView: NSTableView, isGroupRow row: Int) -> Bool {
        rows[row].id == nil
    }

    func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool {
        rows[row].id != nil
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        main?.selectionChanged()
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let item = rows[row]
        if case let .group(name, running, total) = item {
            let cell = reusableCell("group", withImage: false)
            let title = NSMutableAttributedString(string: name, attributes: [.font: NSFont.boldSystemFont(ofSize: 12)])
            title.append(NSAttributedString(
                string: "   \(running) of \(total) running",
                attributes: [.font: NSFont.systemFont(ofSize: 12), .foregroundColor: NSColor.secondaryLabelColor]
            ))
            cell.textField?.attributedStringValue = title
            return cell
        }
        guard let column = tableColumn?.identifier.rawValue else { return nil }
        let dot = column == "name" ? statusDot(item) : nil
        let cell = reusableCell(dot == nil ? "text" : "dotted", withImage: dot != nil)
        cell.textField?.stringValue = text(item, column)
        cell.imageView?.image = dot?.image
        cell.imageView?.contentTintColor = dot?.color
        return cell
    }

    private func text(_ item: ListRow, _ column: String) -> String {
        switch (item, column) {
        case let (.container(container), "name"): container.name
        case let (.container(container), "image"): container.image
        case let (.container(container), "ports"): container.ports
        case let (.container(container), "status"): container.status
        case let (.image(image, _), "name"): image.name
        case let (.image(image, _), "tag"): image.tag
        case let (.image(image, _), "id"): shortID(image.id)
        case let (.image(image, _), "created"): Self.relativeDate.localizedString(for: image.created, relativeTo: .now)
        case let (.image(image, _), "size"): formatBytes(UInt64(max(0, image.size)))
        case let (.network(network, _), "name"): network.name
        case let (.network(network, _), "driver"): network.driver
        case let (.network(network, _), "scope"): network.scope
        case let (.network(_, count), "containers"): "\(count)"
        case let (.network(network, _), "id"): shortID(network.id)
        default: ""
        }
    }

    /// Green for a running container or an image a container uses.
    private func statusDot(_ item: ListRow) -> (image: NSImage?, color: NSColor)? {
        let active: Bool
        switch item {
        case let .container(container): active = container.running
        case let .image(_, inUse): active = inUse
        default: return nil
        }
        let image = NSImage(systemSymbolName: "circle.fill", accessibilityDescription: active ? "Running" : "Stopped")?
            .withSymbolConfiguration(.init(pointSize: 7, weight: .regular))
        return (image, active ? .systemGreen : .tertiaryLabelColor)
    }

    private func reusableCell(_ identifier: String, withImage: Bool) -> NSTableCellView {
        let id = NSUserInterfaceItemIdentifier(identifier)
        if let cell = tableView.makeView(withIdentifier: id, owner: nil) as? NSTableCellView {
            return cell
        }
        let cell = NSTableCellView()
        cell.identifier = id
        layoutCell(cell, withImage: withImage)
        return cell
    }
}

/// Adds a text field, an optional leading image and an optional trailing detail label;
/// shared by the list and the sidebar.
func layoutCell(_ cell: NSTableCellView, withImage: Bool, detail: NSTextField? = nil) {
    let text = NSTextField(labelWithString: "")
    text.lineBreakMode = .byTruncatingTail
    text.translatesAutoresizingMaskIntoConstraints = false
    text.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
    cell.addSubview(text)
    cell.textField = text
    var constraints = [text.centerYAnchor.constraint(equalTo: cell.centerYAnchor)]
    if withImage {
        let image = NSImageView()
        image.translatesAutoresizingMaskIntoConstraints = false
        cell.addSubview(image)
        cell.imageView = image
        constraints += [
            image.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 4),
            image.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
            image.widthAnchor.constraint(equalToConstant: 16),
            text.leadingAnchor.constraint(equalTo: image.trailingAnchor, constant: 6),
        ]
    } else {
        constraints.append(text.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 4))
    }
    if let detail {
        detail.translatesAutoresizingMaskIntoConstraints = false
        detail.setContentHuggingPriority(.required, for: .horizontal)
        cell.addSubview(detail)
        constraints += [
            detail.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
            detail.leadingAnchor.constraint(greaterThanOrEqualTo: text.trailingAnchor, constant: 6),
            detail.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -8),
        ]
    } else {
        constraints.append(text.trailingAnchor.constraint(lessThanOrEqualTo: cell.trailingAnchor, constant: -4))
    }
    NSLayoutConstraint.activate(constraints)
}
