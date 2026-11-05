import AppKit

enum ListRow: Equatable {
    case header(title: String, detail: String)
    case group(project: String, members: [Container], collapsed: Bool)
    case container(Container)
    case image(Image, usedBy: [String])
    case network(Network, attached: [String])

    /// What actions and the detail act on; headers and group rows have none.
    var id: String? {
        switch self {
        case .header, .group: nil
        case let .container(container): container.id
        case let .image(image, _): image.reference
        case let .network(network, _): network.id
        }
    }
}

func listRows(
    _ snapshot: Snapshot, section: Section, onlyRunning: Bool, search: String, collapsed: Set<String> = [], onlyUnused: Bool = false
) -> [ListRow] {
    func matches(_ fields: String...) -> Bool {
        search.isEmpty || fields.contains { $0.range(of: search, options: .caseInsensitive) != nil }
    }
    func counted(_ count: Int, _ noun: String) -> String {
        "\(count) \(noun)\(count == 1 ? "" : "s")"
    }
    switch section {
    case .containers:
        let containers = snapshot.containers.filter {
            (!onlyRunning || $0.running) && matches($0.name, $0.image, $0.project ?? "")
        }
        return groupedByProject(containers, collapsed: collapsed)
    case .images:
        var usedBy: [String: [String]] = [:]
        for container in snapshot.containers.sorted(by: { $0.name < $1.name }) {
            usedBy[container.imageID, default: []].append(container.name)
        }
        let images = snapshot.images.filter { matches($0.reference) }
            .sorted { $0.size != $1.size ? $0.size > $1.size : $0.reference < $1.reference }
        var rows: [ListRow] = []
        for (title, inUse) in [("In use", true), ("Unused", false)] where !(onlyUnused && inUse) {
            let part = images.filter { (usedBy[$0.id] != nil) == inUse }
            guard !part.isEmpty else { continue }
            // Two tags of one image share its bytes, so count each ID once.
            var sizes: [String: Int64] = [:]
            for image in part {
                sizes[image.id] = image.size
            }
            let total = formatBytes(UInt64(max(0, sizes.values.reduce(0, +))))
            rows.append(.header(title: title, detail: "\(counted(part.count, "image")) · \(total)"))
            rows += part.map { .image($0, usedBy: usedBy[$0.id] ?? []) }
        }
        return rows
    case .networks:
        let networks = snapshot.networks.filter { matches($0.name, $0.driver, $0.subnet) }.sorted { $0.name < $1.name }
        var attached: [String: [String]] = [:]
        for container in snapshot.containers.sorted(by: { $0.name < $1.name }) {
            for network in container.networks {
                attached[network, default: []].append(container.name)
            }
        }
        var rows: [ListRow] = []
        for (title, inUse) in [("In use", true), ("Unused", false)] where !(onlyUnused && inUse) {
            let part = networks.filter { (attached[$0.name] != nil) == inUse }
            guard !part.isEmpty else { continue }
            rows.append(.header(title: title, detail: counted(part.count, "network")))
            rows += part.map { .network($0, attached: attached[$0.name] ?? []) }
        }
        return rows
    }
}

/// Compose projects by name with a header each, members hidden while collapsed; containers without a project last, without a header.
func groupedByProject(_ containers: [Container], collapsed: Set<String> = []) -> [ListRow] {
    let sorted = containers.sorted { $0.name < $1.name }
    let projects = Dictionary(grouping: sorted.filter { $0.project != nil }) { $0.project ?? "" }
    var rows: [ListRow] = []
    for (project, members) in projects.sorted(by: { $0.key < $1.key }) {
        let isCollapsed = collapsed.contains(project)
        rows.append(.group(project: project, members: members, collapsed: isCollapsed))
        if !isCollapsed {
            rows += members.map(ListRow.container)
        }
    }
    return rows + sorted.filter { $0.project == nil }.map(ListRow.container)
}

final class ListController: NSViewController, NSTableViewDataSource, NSTableViewDelegate {
    weak var main: MainWindowController?
    let tableView = NSTableView()
    private let placeholder = NSTextField(labelWithString: "")
    private var rows: [ListRow] = []
    /// Newest stats of running containers by ID, for the Containers rows.
    private var live: [String: Sample] = [:]
    private var collapsed: Set<String> = []
    private var polling = false
    private var shownSelection = IndexSet()
    override func loadView() {
        let scroll = NSScrollView()
        scroll.documentView = tableView
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        tableView.style = .inset
        // Every row lays out its own columns.
        tableView.headerView = nil
        tableView.addTableColumn(NSTableColumn(identifier: NSUserInterfaceItemIdentifier("main")))
        tableView.columnAutoresizingStyle = .firstColumnOnlyAutoresizingStyle
        tableView.dataSource = self
        tableView.delegate = self
        tableView.target = self
        tableView.action = #selector(rowClicked)
        tableView.doubleAction = #selector(openClicked)
        let timer = Timer.scheduledTimer(withTimeInterval: 3, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.pollStats() }
        }
        timer.tolerance = 1

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
        tableView.setAccessibilityLabel(main.section.title)
        let selected = rows.indices.contains(tableView.selectedRow) ? selectionKey(rows[tableView.selectedRow]) : nil
        rows = listRows(
            main.snapshot, section: main.section, onlyRunning: main.onlyRunning, search: main.search, collapsed: collapsed,
            onlyUnused: main.onlyUnused
        )
        tableView.reloadData()
        if let selected, let index = rows.firstIndex(where: { selectionKey($0) == selected }) {
            tableView.selectRowIndexes([index], byExtendingSelection: false)
        }
        shownSelection = tableView.selectedRowIndexes
        placeholder.isHidden = !rows.isEmpty
        if main.docker == nil {
            placeholder.stringValue = "No running Docker runtime"
        } else if !main.search.isEmpty || main.onlyRunning || main.onlyUnused {
            placeholder.stringValue = "No matches"
        } else {
            placeholder.stringValue = "No \(main.section.title.lowercased())"
        }
    }

    var selectedID: String? {
        rows.indices.contains(tableView.selectedRow) ? rows[tableView.selectedRow].id : nil
    }

    var selectedProject: String? {
        guard rows.indices.contains(tableView.selectedRow), case let .group(project, _, _) = rows[tableView.selectedRow] else { return nil }
        return project
    }

    /// Compose group rows are selectable too, so the keyboard can fold them.
    private func selectionKey(_ row: ListRow) -> String? {
        if case let .group(project, _, _) = row { return "project:" + project }
        return row.id
    }

    /// Return toggles a selected compose group, Left folds it, Right unfolds it.
    func handleGroupKey(_ keyCode: UInt16) -> Bool {
        let row = tableView.selectedRow
        guard rows.indices.contains(row), case let .group(_, _, isCollapsed) = rows[row] else { return false }
        switch keyCode {
        case 123 where !isCollapsed, 124 where isCollapsed, 36, 76:
            toggleGroup(at: row)
        default:
            break
        }
        return true
    }

    @objc private func openClicked() {
        guard rows.indices.contains(tableView.clickedRow), let id = rows[tableView.clickedRow].id else { return }
        main?.open(id)
    }

    @objc private func rowClicked() {
        toggleGroup(at: tableView.clickedRow)
    }

    @objc private func chevronClicked(_ sender: NSButton) {
        toggleGroup(at: tableView.row(for: sender))
    }

    private func toggleGroup(at row: Int) {
        guard rows.indices.contains(row), case let .group(project, _, isCollapsed) = rows[row] else { return }
        if isCollapsed {
            collapsed.remove(project)
        } else {
            collapsed.insert(project)
        }
        reload()
    }

    func numberOfRows(in tableView: NSTableView) -> Int {
        rows.count
    }

    func tableView(_ tableView: NSTableView, isGroupRow row: Int) -> Bool {
        if case .header = rows[row] { return true }
        return false
    }

    func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool {
        selectionKey(rows[row]) != nil
    }

    func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat {
        if case .header = rows[row] { return 30 }
        return 50
    }

    /// Rows show their action buttons only while selected.
    func tableViewSelectionDidChange(_ notification: Notification) {
        let selection = tableView.selectedRowIndexes
        let changed = shownSelection.symmetricDifference(selection).filteredIndexSet { rows.indices.contains($0) }
        tableView.reloadData(forRowIndexes: changed, columnIndexes: [0])
        shownSelection = selection
        main?.selectionChanged()
    }

    /// One `stream=false` read per running container every 3 s while the window shows; feeds the rows and the sidebar meters.
    private func pollStats() {
        guard
            let main, let docker = main.docker, !polling,
            view.window?.occlusionState.contains(.visible) == true
        else { return }
        let ids = main.snapshot.containers.filter(\.running).map(\.id)
        polling = true
        Task {
            let samples = await withTaskGroup(of: (String, Sample?).self) { group in
                for id in ids {
                    group.addTask { (id, try? await docker.statsOnce(id)) }
                }
                var samples: [String: Sample] = [:]
                for await (id, sample) in group {
                    samples[id] = sample
                }
                return samples
            }
            polling = false
            guard main.docker === docker else { return }
            main.sidebar.showUsage(Array(samples.values))
            live = samples
            showLiveStats()
        }
    }

    private func showLiveStats() {
        for row in rows.indices {
            switch (tableView.view(atColumn: 0, row: row, makeIfNecessary: false), rows[row]) {
            case let (cell as ContainerCell, .container(container)):
                cell.showStats(live[container.id])
            case let (cell as GroupCell, .group(_, members, _)):
                cell.showTotals(members.compactMap { live[$0.id] })
            default:
                break
            }
        }
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let selected = tableView.isRowSelected(row)
        switch rows[row] {
        case let .header(title, detail):
            let cell = reusable("header") { SectionHeaderCell() }
            cell.show(title, detail: detail)
            return cell
        case let .group(project, members, isCollapsed):
            let cell = reusable("group") { GroupCell(toggleTarget: self, action: #selector(chevronClicked), actionTarget: main) }
            cell.show(
                project: project, members: members, collapsed: isCollapsed, latest: members.compactMap { live[$0.id] },
                selected: selected
            )
            return cell
        case let .container(container):
            let cell = reusable("container") { ContainerCell(actionTarget: main) }
            cell.show(container, latest: live[container.id], selected: selected, indented: container.project != nil)
            return cell
        case let .image(image, usedBy):
            let cell = reusable("image") { ResourceCell(.image, actionTarget: main) }
            cell.show(image, usedBy: usedBy, selected: selected)
            return cell
        case let .network(network, attached):
            let cell = reusable("network") { ResourceCell(.network, actionTarget: main) }
            cell.show(network, attached: attached, selected: selected)
            return cell
        }
    }

    private func reusable<Cell: NSView>(_ identifier: String, _ make: () -> Cell) -> Cell {
        let id = NSUserInterfaceItemIdentifier(identifier)
        if let cell = tableView.makeView(withIdentifier: id, owner: nil) as? Cell {
            return cell
        }
        let cell = make()
        cell.identifier = id
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
