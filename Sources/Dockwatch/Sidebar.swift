import AppKit

/// Second line of each sidebar row: "6 running", "8.3 GiB · 6 in use", "2 in use".
func sectionSummary(_ snapshot: Snapshot, _ section: Section) -> String {
    switch section {
    case .containers:
        let running = snapshot.containers.filter(\.running).count
        let stopped = snapshot.containers.count - running
        return stopped == 0 ? "\(running) running" : "\(running) running · \(stopped) stopped"
    case .images:
        let used = Set(snapshot.containers.map(\.imageID))
        // Two tags of one image share its bytes, so count each ID once.
        var sizes: [String: Int64] = [:]
        for image in snapshot.images {
            sizes[image.id] = image.size
        }
        let inUse = snapshot.images.filter { used.contains($0.id) }.count
        return "\(formatBytes(UInt64(max(0, sizes.values.reduce(0, +))))) · \(inUse) in use"
    case .networks:
        let used = Set(snapshot.containers.flatMap(\.networks))
        return "\(snapshot.networks.filter { used.contains($0.name) }.count) in use"
    }
}

/// Rounded fill behind the runtime switcher and the usage meters; VoiceOver reaches only `spokenChildren`, so the text inside is not read twice.
private final class Card: NSView {
    var spokenChildren: [NSView] = []

    init() {
        super.init(frame: .zero)
        wantsLayer = true
        layer?.cornerRadius = 10
        layer?.cornerCurve = .continuous
        layer?.borderWidth = 0.5
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used")
    }

    override func accessibilityChildren() -> [Any]? {
        spokenChildren.compactMap { NSAccessibility.unignoredDescendant(of: $0) }
    }

    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        layer?.backgroundColor = NSColor.quaternarySystemFill.cgColor
        layer?.borderColor = NSColor.separatorColor.cgColor
    }
}

/// Thin bar filled to `fraction`; one accessibility element that speaks its row.
private final class Meter: NSView {
    private let fill = CALayer()
    private let color: NSColor

    init(_ title: String, color: NSColor) {
        self.color = color
        super.init(frame: .zero)
        wantsLayer = true
        layer?.cornerRadius = 2.5
        layer?.addSublayer(fill)
        fill.cornerRadius = 2.5
        heightAnchor.constraint(equalToConstant: 5).isActive = true
        setAccessibilityElement(true)
        setAccessibilityRole(.levelIndicator)
        setAccessibilityLabel(title)
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used")
    }

    var fraction = 0.0 {
        didSet { needsLayout = true }
    }

    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        layer?.backgroundColor = NSColor.quaternaryLabelColor.cgColor
        fill.backgroundColor = color.cgColor
    }

    override func layout() {
        super.layout()
        CATransaction.begin()
        CATransaction.setDisableActions(NSWorkspace.shared.accessibilityDisplayShouldReduceMotion)
        fill.frame = CGRect(x: 0, y: 0, width: bounds.width * min(1, max(0, fraction)), height: bounds.height)
        CATransaction.commit()
    }
}

private final class SidebarCell: NSTableCellView {
    let icon: StatusIcon
    let title = label(13, weight: .medium)
    let subtitle = label(11, secondary: true)
    let count = label(11, secondary: true)

    init(_ section: Section) {
        icon = StatusIcon([.container, .image, .network][section.rawValue], size: 22)
        super.init(frame: .zero)
        icon.layer?.cornerRadius = 6
        icon.active = true
        count.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)
        count.setContentCompressionResistancePriority(.required, for: .horizontal)
        let names = NSStackView(views: [title, subtitle])
        names.orientation = .vertical
        names.alignment = .leading
        names.spacing = 0
        let row = NSStackView()
        row.setViews([icon, names], in: .leading)
        row.setViews([count], in: .trailing)
        row.spacing = 9
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 2),
            row.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -6),
            row.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used")
    }

    /// The label speaks the whole row.
    override func accessibilityChildren() -> [Any]? {
        []
    }
}

/// Docker's CPU percent sums to 100 per busy core, as in `docker stats`.
private struct Usage {
    var cpuPercent = 0.0
    var memory: UInt64 = 0
}

final class SidebarController: NSViewController, NSTableViewDataSource, NSTableViewDelegate {
    private enum Row {
        case header(String)
        case section(Section)
    }

    weak var main: MainWindowController?
    private let tableView = NSTableView()
    private var rows: [Row] = [.header("Resources")] + Section.allCases.map(Row.section)
    private var usage: Usage?

    private let runtimeIcon = StatusIcon(.runtime, size: 30)
    private let runtimeName = label(13, weight: .semibold)
    private let runtimeDetail = label(11, secondary: true)
    private let runtimeDot = NSImageView()
    private let runtimeButton = NSButton(title: "", target: nil, action: nil)

    private let usageCard = Card()
    private let cpuValue = label(11, secondary: true)
    private let cpuMeter = Meter("CPU", color: .systemBlue)
    private let memoryValue = label(11, secondary: true)
    private let memoryMeter = Meter("Memory", color: .systemPurple)
    private let systemLine = label(11, secondary: true)

    override func loadView() {
        let scroll = NSScrollView()
        scroll.documentView = tableView
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        tableView.style = .sourceList
        tableView.headerView = nil
        tableView.addTableColumn(NSTableColumn(identifier: NSUserInterfaceItemIdentifier("main")))
        tableView.dataSource = self
        tableView.delegate = self

        let runtimeCard = makeRuntimeCard()
        makeUsageCard()
        let container = NSView()
        for subview in [runtimeCard, scroll, usageCard] {
            subview.translatesAutoresizingMaskIntoConstraints = false
            container.addSubview(subview)
        }
        NSLayoutConstraint.activate([
            runtimeCard.topAnchor.constraint(equalTo: container.safeAreaLayoutGuide.topAnchor, constant: 6),
            runtimeCard.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: 10),
            runtimeCard.trailingAnchor.constraint(equalTo: container.trailingAnchor, constant: -10),
            scroll.topAnchor.constraint(equalTo: runtimeCard.bottomAnchor, constant: 4),
            scroll.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            scroll.bottomAnchor.constraint(equalTo: usageCard.topAnchor, constant: -8),
            usageCard.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: 10),
            usageCard.trailingAnchor.constraint(equalTo: container.trailingAnchor, constant: -10),
            usageCard.bottomAnchor.constraint(equalTo: container.bottomAnchor, constant: -10),
        ])
        view = container
    }

    /// Icon, name, engine version and a chevron; a transparent button over it opens the runtime menu.
    private func makeRuntimeCard() -> Card {
        let card = Card()
        card.spokenChildren = [runtimeButton]
        let names = NSStackView(views: [runtimeName, runtimeDetail])
        names.orientation = .vertical
        names.alignment = .leading
        names.spacing = 1
        runtimeDot.image = NSImage(systemSymbolName: "circle.fill", accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: 7, weight: .regular))
        let chevron = NSImageView(image: NSImage(systemSymbolName: "chevron.up.chevron.down", accessibilityDescription: nil)!)
        chevron.symbolConfiguration = .init(pointSize: 10, weight: .semibold)
        chevron.contentTintColor = .secondaryLabelColor
        let row = NSStackView()
        row.setViews([runtimeIcon, names], in: .leading)
        row.setViews([runtimeDot, chevron], in: .trailing)
        row.spacing = 9

        runtimeButton.isBordered = false
        runtimeButton.target = self
        runtimeButton.action = #selector(showRuntimeMenu)
        runtimeButton.setAccessibilityRole(.popUpButton)
        runtimeButton.setAccessibilityHelp("Switch runtime, Command 1 to 9")
        for subview in [row, runtimeButton] {
            subview.translatesAutoresizingMaskIntoConstraints = false
            card.addSubview(subview)
        }
        NSLayoutConstraint.activate([
            row.topAnchor.constraint(equalTo: card.topAnchor, constant: 8),
            row.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -8),
            row.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 8),
            row.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -10),
            runtimeButton.topAnchor.constraint(equalTo: card.topAnchor),
            runtimeButton.bottomAnchor.constraint(equalTo: card.bottomAnchor),
            runtimeButton.leadingAnchor.constraint(equalTo: card.leadingAnchor),
            runtimeButton.trailingAnchor.constraint(equalTo: card.trailingAnchor),
        ])
        return card
    }

    /// CPU and memory of all running containers against the engine's CPUs and RAM, then the VM's OS.
    private func makeUsageCard() {
        func meterRow(_ title: String, value: NSTextField, meter: Meter) -> NSStackView {
            let name = label(11)
            name.stringValue = title
            name.setContentCompressionResistancePriority(.required, for: .horizontal)
            value.alignment = .right
            let line = NSStackView()
            line.setViews([name], in: .leading)
            line.setViews([value], in: .trailing)
            let row = NSStackView(views: [line, meter])
            row.orientation = .vertical
            row.alignment = .leading
            row.spacing = 4
            line.widthAnchor.constraint(equalTo: row.widthAnchor).isActive = true
            meter.widthAnchor.constraint(equalTo: row.widthAnchor).isActive = true
            return row
        }
        let rows = [meterRow("CPU", value: cpuValue, meter: cpuMeter), meterRow("Memory", value: memoryValue, meter: memoryMeter), systemLine]
        let stack = NSStackView(views: rows)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 9
        for row in rows {
            row.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        usageCard.spokenChildren = [cpuMeter, memoryMeter, systemLine]
        usageCard.setAccessibilityElement(true)
        usageCard.setAccessibilityRole(.group)
        usageCard.setAccessibilityLabel("Engine usage")
        stack.translatesAutoresizingMaskIntoConstraints = false
        usageCard.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: usageCard.topAnchor, constant: 10),
            stack.bottomAnchor.constraint(equalTo: usageCard.bottomAnchor, constant: -10),
            stack.leadingAnchor.constraint(equalTo: usageCard.leadingAnchor, constant: 10),
            stack.trailingAnchor.constraint(equalTo: usageCard.trailingAnchor, constant: -10),
        ])
    }

    func reload() {
        guard let main else { return }
        loadViewIfNeeded()
        if main.docker == nil {
            usage = nil
        }
        tableView.reloadData()
        showRuntime()
        showUsage()
        if let index = rows.firstIndex(where: { if case .section(main.section) = $0 { true } else { false } }) {
            tableView.selectRowIndexes([index], byExtendingSelection: false)
        }
    }

    /// Takes the newest stats poll of all running containers.
    func showUsage(_ samples: [Sample]) {
        var total = Usage()
        for sample in samples {
            total.cpuPercent += sample.cpuPercent ?? 0
            total.memory += sample.memoryUsed
        }
        usage = total
        showUsage()
    }

    private func showRuntime() {
        guard let main else { return }
        let runtime = main.runtimes.first { $0.socket == main.chosenSocket }
        let running = runtime?.running == true
        runtimeName.stringValue = runtime?.name ?? "No runtime"
        runtimeDetail.stringValue = switch (runtime, main.engine) {
        case (nil, _): "No Docker socket found"
        case let (_, engine?): "Engine \(engine.version)"
        case _ where running: "Connecting"
        default: "Stopped"
        }
        runtimeIcon.active = running
        runtimeDot.contentTintColor = running ? .systemGreen : .tertiaryLabelColor
        runtimeButton.toolTip = runtime?.socket
        runtimeButton.setAccessibilityLabel("Runtime \(runtimeName.stringValue), \(running ? "running" : "stopped")")
        runtimeButton.setAccessibilityValue(runtimeDetail.stringValue)
    }

    private func showUsage() {
        guard let engine = main?.engine else {
            usageCard.isHidden = true
            return
        }
        usageCard.isHidden = false
        let cpus = max(1, engine.cpus)
        let memory = UInt64(max(1, engine.memory))
        let cpu = usage.map { String(format: "%.1f%%", $0.cpuPercent / Double(cpus)) } ?? "…"
        let used = usage.map { formatBytes($0.memory) } ?? "…"
        cpuValue.stringValue = "\(cpu) of \(cpus) CPUs"
        memoryValue.stringValue = "\(used) of \(formatBytes(memory))"
        cpuMeter.fraction = (usage?.cpuPercent ?? 0) / Double(cpus * 100)
        memoryMeter.fraction = Double(usage?.memory ?? 0) / Double(memory)
        cpuMeter.setAccessibilityValue(cpuValue.stringValue)
        memoryMeter.setAccessibilityValue(memoryValue.stringValue)
        systemLine.stringValue = "\(engine.system) · \(engine.architecture)"
        systemLine.setAccessibilityLabel("Engine \(engine.version) on \(engine.system), \(engine.architecture)")
    }

    @objc private func showRuntimeMenu() {
        guard let main else { return }
        let menu = NSMenu()
        for (index, runtime) in main.runtimes.enumerated() {
            let item = NSMenuItem(title: runtime.name, action: #selector(runtimeChosen), keyEquivalent: index < 9 ? "\(index + 1)" : "")
            item.target = self
            item.tag = index
            item.subtitle = "\(runtime.running ? "Running" : "Stopped") · \((runtime.socket as NSString).abbreviatingWithTildeInPath)"
            item.state = runtime.socket == main.chosenSocket ? .on : .off
            menu.addItem(item)
        }
        if menu.items.isEmpty {
            menu.addItem(NSMenuItem(title: "No Docker sockets found", action: nil, keyEquivalent: ""))
        }
        menu.addItem(.separator())
        let rescan = NSMenuItem(title: "Rescan Runtimes", action: #selector(MainWindowController.rescan), keyEquivalent: "")
        rescan.target = main
        menu.addItem(rescan)
        // NSButton is flipped, so the card's bottom edge is at bounds.height.
        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: runtimeButton.bounds.height + 4), in: runtimeButton)
    }

    @objc private func runtimeChosen(_ item: NSMenuItem) {
        main?.switchRuntime(item.tag)
    }

    func numberOfRows(in tableView: NSTableView) -> Int {
        rows.count
    }

    func tableView(_ tableView: NSTableView, isGroupRow row: Int) -> Bool {
        if case .header = rows[row] { true } else { false }
    }

    func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat {
        if case .header = rows[row] { 26 } else { 44 }
    }

    func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool {
        if case .section = rows[row] { true } else { false }
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
        switch rows[row] {
        case let .header(title):
            let cell = reusable("header") { () -> NSTableCellView in
                let cell = NSTableCellView()
                layoutCell(cell, withImage: false)
                return cell
            }
            cell.textField?.stringValue = title
            return cell
        case let .section(section):
            let cell = reusable("section-\(section.rawValue)") { SidebarCell(section) }
            let connected = main.docker != nil
            let count = switch section {
            case .containers: main.snapshot.containers.count
            case .images: main.snapshot.images.count
            case .networks: main.snapshot.networks.count
            }
            cell.title.stringValue = section.title
            cell.subtitle.stringValue = connected ? sectionSummary(main.snapshot, section) : "Not connected"
            cell.count.stringValue = connected ? "\(count)" : ""
            cell.setAccessibilityLabel(
                ([section.title, cell.count.stringValue] + cell.subtitle.stringValue.components(separatedBy: " · "))
                    .filter { !$0.isEmpty }.joined(separator: ", ")
            )
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
