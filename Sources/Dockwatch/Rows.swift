import AppKit

/// Gradient badge with a white symbol per row kind; gray while inactive (stopped, unused).
final class StatusIcon: NSView {
    enum Kind { case container, project, image, network, runtime }

    private let gradient = CAGradientLayer()
    private let symbol = NSImageView()
    private let colors: (NSColor, NSColor)

    init(_ kind: Kind, size: CGFloat = 30) {
        let name: String
        switch kind {
        case .container:
            name = "shippingbox.fill"
            colors = (NSColor(srgbRed: 0.24, green: 0.55, blue: 1, alpha: 1), NSColor(srgbRed: 0.08, green: 0.31, blue: 0.86, alpha: 1))
        case .project:
            name = "square.stack.3d.up.fill"
            colors = (NSColor(srgbRed: 0.37, green: 0.36, blue: 0.9, alpha: 1), NSColor(srgbRed: 0.21, green: 0.2, blue: 0.66, alpha: 1))
        case .image:
            name = "square.3.layers.3d"
            colors = (NSColor(srgbRed: 0.25, green: 0.78, blue: 0.88, alpha: 1), NSColor(srgbRed: 0.06, green: 0.54, blue: 0.66, alpha: 1))
        case .network:
            name = "network"
            colors = (NSColor(srgbRed: 0.2, green: 0.78, blue: 0.35, alpha: 1), NSColor(srgbRed: 0.11, green: 0.54, blue: 0.24, alpha: 1))
        case .runtime:
            name = "server.rack"
            colors = (NSColor(srgbRed: 0.5, green: 0.54, blue: 0.62, alpha: 1), NSColor(srgbRed: 0.27, green: 0.3, blue: 0.37, alpha: 1))
        }
        super.init(frame: .zero)
        wantsLayer = true
        layer = gradient
        gradient.cornerRadius = kind == .container ? size / 2 : size * 0.3
        gradient.cornerCurve = .continuous
        symbol.image = NSImage(systemSymbolName: name, accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: size * 0.43, weight: .medium))
        symbol.translatesAutoresizingMaskIntoConstraints = false
        addSubview(symbol)
        NSLayoutConstraint.activate([
            widthAnchor.constraint(equalToConstant: size),
            heightAnchor.constraint(equalToConstant: size),
            symbol.centerXAnchor.constraint(equalTo: centerXAnchor),
            symbol.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used")
    }

    var active = false {
        didSet { updateColors() }
    }

    override func viewDidChangeEffectiveAppearance() {
        updateColors()
    }

    private func updateColors() {
        // Layer colors do not follow the appearance on their own.
        effectiveAppearance.performAsCurrentDrawingAppearance {
            let (light, dark) = active ? colors : (NSColor.quaternaryLabelColor, NSColor.quaternaryLabelColor)
            // Top to bottom in layer coordinates, which start at the bottom.
            gradient.colors = [dark.cgColor, light.cgColor]
        }
        symbol.contentTintColor = active ? .white : .secondaryLabelColor
    }
}

func label(_ size: CGFloat, weight: NSFont.Weight = .regular, secondary: Bool = false) -> NSTextField {
    let field = NSTextField(labelWithString: "")
    field.font = .systemFont(ofSize: size, weight: weight)
    field.textColor = secondary ? .secondaryLabelColor : .labelColor
    field.lineBreakMode = .byTruncatingTail
    field.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
    return field
}

/// Right-aligned fixed-width figure, so CPU and memory line up in every row.
private func figure(width: CGFloat) -> NSTextField {
    let field = NSTextField(labelWithString: "")
    field.font = .monospacedDigitSystemFont(ofSize: 12, weight: .regular)
    field.textColor = .secondaryLabelColor
    field.alignment = .right
    field.lineBreakMode = .byTruncatingTail
    field.widthAnchor.constraint(equalToConstant: width).isActive = true
    return field
}

private func actionButton(_ symbol: String, _ title: String, _ target: AnyObject?, _ action: Selector) -> NSButton {
    let button = NSButton(
        image: NSImage(systemSymbolName: symbol, accessibilityDescription: title)!, target: target, action: action
    )
    button.bezelStyle = .accessoryBarAction
    button.toolTip = title
    return button
}

/// Shared row layout: indent, icon, title over subtitle, then the trailing views.
private func layoutRow(_ cell: RowCell, icon: NSView, title: NSView, subtitle: NSTextField, trailing: [NSView]) -> NSLayoutConstraint {
    let names = NSStackView(views: [title, subtitle])
    names.orientation = .vertical
    names.alignment = .leading
    names.spacing = 1
    names.setContentHuggingPriority(.defaultLow, for: .horizontal)
    let row = NSStackView()
    row.setViews([icon, names], in: .leading)
    row.setViews(trailing, in: .trailing)
    row.spacing = 11
    row.translatesAutoresizingMaskIntoConstraints = false
    cell.addSubview(row)
    cell.setAccessibilityElement(true)
    cell.setAccessibilityRole(.group)
    let leading = row.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 6)
    NSLayoutConstraint.activate([
        leading,
        row.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -8),
        row.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
    ])
    return leading
}

private func formatCPU(_ sample: Sample?) -> String {
    sample?.cpuPercent.map { String(format: "%.1f%%", $0) } ?? ""
}

/// "CPU 1.2%, memory 40 MB" for the accessibility label; empty without a sample.
private func spokenStats(cpu: String, memory: String) -> [String] {
    cpu.isEmpty ? [] : ["CPU \(cpu)", "memory \(memory)"]
}

/// One accessibility element per row: its label speaks the whole row, and only the buttons stay reachable inside.
class RowCell: NSTableCellView {
    var buttons: [NSButton] = []

    override func accessibilityChildren() -> [Any]? {
        buttons.filter { !$0.isHiddenOrHasHiddenAncestor }.compactMap { NSAccessibility.unignoredDescendant(of: $0) }
    }
}

final class ContainerCell: RowCell {
    static let cpuWidth: CGFloat = 64
    static let memoryWidth: CGFloat = 72
    private let icon = StatusIcon(.container)
    private let name = label(13, weight: .semibold)
    private let subtitle = label(11.5, secondary: true)
    private let cpu = figure(width: cpuWidth)
    private let memory = figure(width: memoryWidth)
    private let actions = NSStackView()
    private let openPort: NSButton
    private let start: NSButton
    private let stop: NSButton
    private var leading = NSLayoutConstraint()
    private var port: String?
    private var summary: [String] = []

    init(actionTarget: MainWindowController?) {
        openPort = actionButton("arrow.up.right.square", "Open in Browser", nil, #selector(openInBrowser))
        start = actionButton("play.fill", "Start", actionTarget, #selector(MainWindowController.startContainer(_:)))
        stop = actionButton("stop.fill", "Stop", actionTarget, #selector(MainWindowController.stopContainer(_:)))
        let restart = actionButton("arrow.clockwise", "Restart", actionTarget, #selector(MainWindowController.restartContainer(_:)))
        let remove = actionButton("trash", "Remove", actionTarget, #selector(MainWindowController.removeSelected(_:)))
        super.init(frame: .zero)
        openPort.target = self
        textField = name
        buttons = [openPort, start, stop, restart, remove]
        for view in buttons {
            actions.addArrangedSubview(view)
        }
        actions.spacing = 2
        actions.setHuggingPriority(.required, for: .horizontal)
        leading = layoutRow(self, icon: icon, title: name, subtitle: subtitle, trailing: [actions, cpu, memory])
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used")
    }

    /// `indented` for members of a compose project, under their header.
    func show(_ container: Container, latest: Sample?, selected: Bool, indented: Bool) {
        leading.constant = indented ? 30 : 6
        icon.active = container.running
        name.stringValue = container.name
        subtitle.stringValue = container.ports.isEmpty ? container.image : "\(container.image) · \(container.ports)"
        subtitle.toolTip = container.status
        summary = [container.name, container.status, container.image]
        if !container.ports.isEmpty {
            summary.append("ports \(container.ports)")
        }
        showStats(latest)

        // "8080:80, 5432:5432" opens the first published host port.
        port = container.ports.split(separator: ",").first?.split(separator: ":").first.map(String.init)
        actions.isHidden = !selected
        openPort.isHidden = port == nil || !container.running
        start.isHidden = container.running
        stop.isHidden = !container.running
    }

    /// Updates only the figures, so a stats tick does not rebuild the row.
    func showStats(_ latest: Sample?) {
        cpu.stringValue = formatCPU(latest)
        memory.stringValue = latest.map { formatBytes($0.memoryUsed) } ?? ""
        setAccessibilityLabel((summary + spokenStats(cpu: cpu.stringValue, memory: memory.stringValue)).joined(separator: ", "))
    }

    @objc private func openInBrowser() {
        guard let port, let url = URL(string: "http://localhost:\(port)") else { return }
        NSWorkspace.shared.open(url)
    }
}

final class GroupCell: RowCell {
    private let chevron = NSButton()
    private let icon = StatusIcon(.project)
    private let title = label(13, weight: .semibold)
    private let summary = label(11.5, secondary: true)
    private let cpu = figure(width: ContainerCell.cpuWidth)
    private let memory = figure(width: ContainerCell.memoryWidth)
    private var spoken: [String] = []

    init(toggleTarget: AnyObject, action: Selector) {
        super.init(frame: .zero)
        textField = title
        chevron.isBordered = false
        chevron.imagePosition = .imageOnly
        chevron.target = toggleTarget
        chevron.action = action
        chevron.contentTintColor = .secondaryLabelColor
        chevron.translatesAutoresizingMaskIntoConstraints = false
        addSubview(chevron)
        buttons = [chevron]
        let leading = layoutRow(self, icon: icon, title: title, subtitle: summary, trailing: [cpu, memory])
        leading.constant = 24
        NSLayoutConstraint.activate([
            chevron.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 6),
            chevron.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used")
    }

    func show(project: String, members: [Container], collapsed: Bool, latest: [Sample]) {
        let running = members.filter(\.running).count
        let toggle = collapsed ? "Expand \(project)" : "Collapse \(project)"
        chevron.image = NSImage(
            systemSymbolName: collapsed ? "chevron.right" : "chevron.down", accessibilityDescription: toggle
        )?.withSymbolConfiguration(.init(pointSize: 10, weight: .semibold))
        icon.active = running > 0
        title.stringValue = project
        summary.stringValue = "\(members.count) containers · " + (running == 0 ? "stopped" : "\(running) running")
        spoken = [
            "\(project), compose project", summary.stringValue.replacingOccurrences(of: " · ", with: ", "),
            collapsed ? "collapsed" : "expanded",
        ]
        showTotals(latest)
    }

    /// `latest` holds the newest sample of each running member; empty clears the totals.
    func showTotals(_ latest: [Sample]) {
        var total = 0.0
        var used: UInt64 = 0
        for sample in latest {
            total += sample.cpuPercent ?? 0
            used += sample.memoryUsed
        }
        cpu.stringValue = latest.isEmpty ? "" : String(format: "%.1f%%", total)
        memory.stringValue = latest.isEmpty ? "" : formatBytes(used)
        setAccessibilityLabel((spoken + spokenStats(cpu: cpu.stringValue, memory: memory.stringValue)).joined(separator: ", "))
    }
}

/// Image or network row: icon, name with an optional "built in" badge, subtitle, users, size.
final class ResourceCell: RowCell {
    private static let relativeDate = RelativeDateTimeFormatter()
    private let icon: StatusIcon
    private let name = label(13, weight: .semibold)
    private let badge = NSBox()
    private let subtitle = label(11.5, secondary: true)
    private let users = figure(width: 170)
    private let size = figure(width: ContainerCell.memoryWidth)
    private let remove: NSButton

    init(_ kind: StatusIcon.Kind, actionTarget: MainWindowController?) {
        icon = StatusIcon(kind)
        remove = actionButton("trash", "Remove", actionTarget, #selector(MainWindowController.removeSelected(_:)))
        super.init(frame: .zero)
        textField = name
        let badgeText = label(10.5, weight: .medium, secondary: true)
        badgeText.stringValue = "built in"
        badgeText.translatesAutoresizingMaskIntoConstraints = false
        badge.boxType = .custom
        badge.borderColor = .tertiaryLabelColor
        badge.cornerRadius = 5
        badge.contentViewMargins = .zero
        badge.contentView?.addSubview(badgeText)
        if let content = badge.contentView {
            NSLayoutConstraint.activate([
                badgeText.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 5),
                badgeText.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -5),
                badgeText.topAnchor.constraint(equalTo: content.topAnchor, constant: 1),
                badgeText.bottomAnchor.constraint(equalTo: content.bottomAnchor, constant: -1),
            ])
        }
        let title = NSStackView(views: [name, badge])
        title.spacing = 6
        _ = layoutRow(self, icon: icon, title: title, subtitle: subtitle, trailing: [remove, users, size])
        buttons = [remove]
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used")
    }

    func show(_ image: Image, usedBy: [String], selected: Bool) {
        let age = Self.relativeDate.localizedString(for: image.created, relativeTo: .now)
        let bytes = formatBytes(UInt64(max(0, image.size)))
        show(
            image.reference, active: !usedBy.isEmpty, users: usedBy, size: bytes,
            subtitle: [shortID(image.id), age], removable: selected && usedBy.isEmpty
        )
        setAccessibilityLabel(
            [image.reference, usedBy.isEmpty ? "unused" : "used by \(usedBy.joined(separator: ", "))", bytes, "created \(age)"]
                .joined(separator: ", ")
        )
    }

    func show(_ network: Network, attached: [String], selected: Bool) {
        let subtitle = [network.driver, network.subnet].filter { !$0.isEmpty }
        show(
            network.name, active: !attached.isEmpty, users: attached, size: nil,
            subtitle: subtitle, removable: selected && !network.isSystem
        )
        badge.isHidden = !network.isSystem
        var spoken = [network.name] + (network.isSystem ? ["built in"] : []) + ["driver \(network.driver)"]
        if !network.subnet.isEmpty {
            spoken.append("subnet \(network.subnet)")
        }
        spoken.append(attached.isEmpty ? "no containers attached" : "attached \(attached.joined(separator: ", "))")
        setAccessibilityLabel(spoken.joined(separator: ", "))
    }

    private func show(_ title: String, active: Bool, users names: [String], size bytes: String?, subtitle parts: [String], removable: Bool) {
        icon.active = active
        name.stringValue = title
        name.toolTip = title
        badge.isHidden = true
        subtitle.stringValue = parts.joined(separator: " · ")
        // "vapp-web, vapp-db +3": the tooltip and the accessibility label carry the full list.
        users.stringValue = names.count <= 2 ? names.joined(separator: ", ") : names.prefix(2).joined(separator: ", ") + " +\(names.count - 2)"
        users.toolTip = names.joined(separator: "\n")
        size.isHidden = bytes == nil
        size.stringValue = bytes ?? ""
        remove.isHidden = !removable
    }
}

/// "In use · 6 images · 1.9 GB" above the Images and Networks rows.
final class SectionHeaderCell: RowCell {
    private let title = label(12, weight: .semibold)
    private let detail = label(11, secondary: true)

    override init(frame: NSRect) {
        super.init(frame: frame)
        textField = title
        let row = NSStackView(views: [title, detail])
        row.spacing = 8
        row.alignment = .firstBaseline
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        setAccessibilityElement(true)
        setAccessibilityRole(.staticText)
        setAccessibilityRoleDescription("heading")
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8),
            row.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -8),
            row.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -4),
        ])
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used")
    }

    func show(_ text: String, detail info: String) {
        title.stringValue = text
        detail.stringValue = info
        // VoiceOver reads a static text's value, not its label.
        setAccessibilityValue("\(text), \(info)")
    }
}
