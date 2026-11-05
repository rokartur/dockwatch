import AppKit

enum Section: Int, CaseIterable {
    case containers, images, networks

    var title: String { ["Containers", "Images", "Networks"][rawValue] }
}

enum Pane: Int, CaseIterable {
    case logs, stats, files, inspect, env

    var title: String { ["Logs", "Stats", "Files", "Inspect", "Env"][rawValue] }
}

private extension NSToolbarItem.Identifier {
    static let back = Self("back")
    static let start = Self("start")
    static let stop = Self("stop")
    static let restart = Self("restart")
    static let remove = Self("remove")
    static let terminal = Self("terminal")
    static let pull = Self("pull")
    static let prune = Self("prune")
    static let panes = Self("panes")
    static let runningFilter = Self("runningFilter")
    static let unusedFilter = Self("unusedFilter")
    static let actionSpacer = Self("actionSpacer")
    static let search = Self("search")
    static let logFilter = Self("logFilter")
}

/// Owns the app state: runtimes, the connection, the snapshot, and what the window shows.
/// The sidebar, list and detail controllers read it and call back into it.
final class MainWindowController: NSWindowController, NSToolbarDelegate, NSToolbarItemValidation, NSMenuItemValidation {
    private(set) var runtimes: [Runtime] = []
    private(set) var chosenSocket: String?
    private(set) var docker: Docker?
    private(set) var engine: Engine?
    private(set) var snapshot = Snapshot()
    private(set) var section = Section.containers
    private(set) var onlyRunning = false
    private(set) var onlyUnused = false
    private(set) var search = ""
    /// Item whose detail replaces the list: a container ID, image reference or network ID.
    private(set) var openID: String?
    private var pulling: String?
    private var events: DockerStream?
    private var refreshPending = false
    private var rescanTimer: Timer?
    private var keyMonitor: Any?

    let sidebar = SidebarController()
    private let list = ListController()
    private let detail = DetailController()
    private let content = NSTabViewController()
    private var toolbarItems: [NSToolbarItem.Identifier: NSToolbarItem] = [:]

    init() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1200, height: 760),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered, defer: false
        )
        window.minSize = NSSize(width: 900, height: 560)
        window.toolbarStyle = .unified
        window.title = "Dockwatch"
        super.init(window: window)

        sidebar.main = self
        list.main = self
        detail.main = self
        content.tabStyle = .unspecified
        content.transitionOptions = []
        content.addChild(list)
        content.addChild(detail)

        let split = NSSplitViewController()
        let sidebarItem = NSSplitViewItem(sidebarWithViewController: sidebar)
        sidebarItem.minimumThickness = 200
        sidebarItem.maximumThickness = 300
        sidebarItem.canCollapse = false
        split.addSplitViewItem(sidebarItem)
        let contentItem = NSSplitViewItem(viewController: content)
        if #available(macOS 26, *) {
            // Lets the sidebar float as glass over the content instead of sitting beside it.
            contentItem.automaticallyAdjustsSafeAreaInsets = true
        }
        split.addSplitViewItem(contentItem)
        window.contentViewController = split
        window.setContentSize(NSSize(width: 1200, height: 760))
        window.center()
        window.setFrameAutosaveName("Main")

        let toolbar = NSToolbar(identifier: "main")
        toolbar.delegate = self
        toolbar.displayMode = .iconOnly
        window.toolbar = toolbar

        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            MainActor.assumeIsolated { self?.handleKey(event) == true } ? nil : event
        }
        rescanTimer = Timer.scheduledTimer(withTimeInterval: 5, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.rescan(nil) }
        }
        sidebar.reload()
        list.reload()
        updateChrome()
        window.makeFirstResponder(list.tableView)
        rescan(nil)
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used")
    }

    // MARK: Runtimes and connection

    @objc func rescan(_ sender: Any?) {
        Task {
            applyRuntimes(await discoverRuntimes())
        }
        // Docker's status text ("Up 3 minutes") ages without any event.
        scheduleRefresh()
    }

    /// Keeps the chosen runtime by socket. With no choice yet, picks the current Docker context
    /// or the first running runtime. Connects once the chosen one runs, disconnects when it stops.
    private func applyRuntimes(_ found: [Runtime]) {
        if found != runtimes {
            runtimes = found
            sidebar.reload()
        }
        if chosenSocket == nil {
            chosenSocket = (found.first { $0.current && $0.running } ?? found.first { $0.running })?.socket
        }
        let running = found.first { $0.socket == chosenSocket }?.running ?? false
        if running, docker == nil, let chosenSocket {
            connect(chosenSocket)
        } else if !running, docker != nil {
            disconnect()
        }
    }

    func switchRuntime(_ index: Int) {
        guard runtimes.indices.contains(index) else { return }
        let runtime = runtimes[index]
        guard runtime.socket != chosenSocket || docker == nil else { return }
        chosenSocket = runtime.socket
        disconnect()
        if runtime.running {
            connect(runtime.socket)
        }
        sidebar.reload()
    }

    private func connect(_ socket: String) {
        let docker = Docker(socket: socket)
        self.docker = docker
        do {
            events = try docker.events(
                onEvent: { [weak self] in self?.scheduleRefresh() },
                onEnd: { [weak self] _ in
                    // The runtime stopped or its socket went away; the 5 s rescan reconnects.
                    if self?.docker === docker { self?.disconnect() }
                }
            )
        } catch {
            disconnect()
            return
        }
        refresh()
        Task {
            let engine = try? await docker.engine()
            guard self.docker === docker else { return }
            self.engine = engine
            sidebar.reload()
        }
    }

    private func disconnect() {
        events?.cancel()
        events = nil
        docker = nil
        engine = nil
        snapshot = Snapshot()
        closeDetail()
        sidebar.reload()
        list.reload()
        updateChrome()
    }

    /// Events come in bursts (a compose up sends dozens); one snapshot per 150 ms is enough.
    private func scheduleRefresh() {
        guard !refreshPending else { return }
        refreshPending = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.15) { [weak self] in
            self?.refreshPending = false
            self?.refresh()
        }
    }

    private func refresh() {
        guard let docker else { return }
        Task {
            do {
                let snapshot = try await docker.snapshot()
                guard self.docker === docker, snapshot != self.snapshot else { return }
                apply(snapshot)
            } catch {
                guard self.docker === docker else { return }
                showError(error)
            }
        }
    }

    private func apply(_ newSnapshot: Snapshot) {
        let wasRunning = openContainer?.running
        snapshot = newSnapshot
        if let openID, !exists(openID) {
            closeDetail()
        } else if wasRunning == false, openContainer?.running == true {
            detail.reopenStreams()
        }
        sidebar.reload()
        list.reload()
        updateChrome()
    }

    private func exists(_ id: String) -> Bool {
        switch section {
        case .containers: snapshot.containers.contains { $0.id == id }
        case .images: snapshot.images.contains { $0.reference == id }
        case .networks: snapshot.networks.contains { $0.id == id }
        }
    }

    // MARK: Navigation

    @objc func showContainers(_ sender: Any?) { showSection(.containers) }
    @objc func showImages(_ sender: Any?) { showSection(.images) }
    @objc func showNetworks(_ sender: Any?) { showSection(.networks) }

    func showSection(_ newSection: Section) {
        closeDetail()
        guard newSection != section else { return }
        section = newSection
        search = ""
        (toolbarItems[.search] as? NSSearchToolbarItem)?.searchField.stringValue = ""
        sidebar.reload()
        list.reload()
        updateChrome()
    }

    func open(_ id: String) {
        guard let docker else { return }
        openID = id
        (toolbarItems[.logFilter] as? NSSearchToolbarItem)?.searchField.stringValue = ""
        detail.open(section, id: id, docker: docker)
        content.selectedTabViewItemIndex = 1
        updateChrome()
        window?.makeFirstResponder(detail.focusView)
    }

    @objc func goBack(_ sender: Any?) {
        closeDetail()
    }

    private func closeDetail() {
        guard openID != nil else { return }
        openID = nil
        detail.close()
        content.selectedTabViewItemIndex = 0
        updateChrome()
        window?.makeFirstResponder(list.tableView)
    }

    func showPane(_ pane: Pane) {
        guard openContainer != nil else { return }
        detail.show(pane)
        updateChrome()
    }

    @objc private func paneChanged(_ sender: NSToolbarItemGroup) {
        showPane(Pane(rawValue: sender.selectedIndex) ?? .logs)
    }

    @objc private func runningFilterChanged(_ sender: NSToolbarItemGroup) {
        onlyRunning = sender.selectedIndex == 1
        list.reload()
    }

    @objc private func unusedFilterChanged(_ sender: NSToolbarItemGroup) {
        onlyUnused = sender.selectedIndex == 1
        list.reload()
    }

    @objc func toggleOnlyRunning(_ sender: Any?) {
        guard section == .containers, openID == nil else { return }
        onlyRunning.toggle()
        (toolbarItems[.runningFilter] as? NSToolbarItemGroup)?.selectedIndex = onlyRunning ? 1 : 0
        list.reload()
    }

    @objc private func searchChanged(_ sender: NSSearchField) {
        search = sender.stringValue
        list.reload()
    }

    @objc private func logFilterChanged(_ sender: NSSearchField) {
        detail.setFilter(sender.stringValue)
    }

    /// `/` filters the logs inside a container, and searches the list everywhere else.
    @objc func focusSearch(_ sender: Any?) {
        if openContainer != nil {
            showPane(.logs)
            window?.makeFirstResponder((toolbarItems[.logFilter] as? NSSearchToolbarItem)?.searchField)
        } else if openID == nil {
            window?.makeFirstResponder((toolbarItems[.search] as? NSSearchToolbarItem)?.searchField)
        }
    }

    @objc func toggleTimestamps(_ sender: Any?) {
        detail.toggleTimestamps()
    }

    @objc func showPalette(_ sender: Any?) {
        guard let view = window?.contentView else { return }
        let commands: [(String, Selector)] = [
            ("Go to Containers", #selector(showContainers)),
            ("Go to Images", #selector(showImages)),
            ("Go to Networks", #selector(showNetworks)),
            ("Start or Stop Container", #selector(toggleStart)),
            ("Restart Container", #selector(restartContainer)),
            ("Open in Terminal", #selector(openInTerminal)),
            ("Remove Selected", #selector(removeSelected)),
            ("Search or Filter Logs", #selector(focusSearch)),
            ("Show Only Running Containers", #selector(toggleOnlyRunning)),
            ("Toggle Log Timestamps", #selector(toggleTimestamps)),
            ("Pull Image…", #selector(pullImage)),
            ("Remove Unused Images", #selector(pruneImages)),
            ("Remove Unused Networks", #selector(pruneNetworks)),
            ("Rescan Runtimes", #selector(rescan)),
        ]
        let menu = NSMenu(title: "Commands")
        for (title, action) in commands {
            menu.addItem(withTitle: title, action: action, keyEquivalent: "").target = self
        }
        menu.popUp(positioning: nil, at: NSPoint(x: view.bounds.midX - 120, y: view.bounds.midY + 120), in: view)
    }

    // MARK: Actions

    private var targetID: String? { openID ?? list.selectedID }

    private var targetContainer: Container? {
        guard section == .containers, let id = targetID else { return nil }
        return snapshot.containers.first { $0.id == id }
    }

    private var openContainer: Container? {
        guard section == .containers, let openID else { return nil }
        return snapshot.containers.first { $0.id == openID }
    }

    @objc func toggleStart(_ sender: Any?) {
        guard let container = targetContainer else { return }
        container.running ? stopContainer(sender) : startContainer(sender)
    }

    @objc func openInTerminal(_ sender: Any?) {
        guard let docker, let container = targetContainer else { return }
        do {
            try openTerminal(socket: docker.socket, containerID: container.id)
        } catch {
            showError(error)
        }
    }

    @objc func startContainer(_ sender: Any?) { runOnTarget { try await $0.start($1) } }
    @objc func stopContainer(_ sender: Any?) { runOnTarget { try await $0.stop($1) } }
    @objc func restartContainer(_ sender: Any?) { runOnTarget { try await $0.restart($1) } }

    private func runOnTarget(_ action: @escaping (Docker, String) async throws -> Void) {
        guard let docker, let container = targetContainer else { return }
        Task {
            do {
                try await action(docker, container.id)
            } catch {
                showError(error)
            }
        }
    }

    @objc func removeSelected(_ sender: Any?) {
        guard let docker, let window, let id = targetID, canRemoveTarget else { return }
        let section = section
        let alert = NSAlert()
        var force = false
        switch section {
        case .containers:
            guard let container = targetContainer else { return }
            force = container.running
            alert.messageText = "Remove \(container.name)?"
            alert.informativeText = container.running
                ? "The container is running. It will be killed, then removed. Its volumes stay."
                : "The container is removed. Its volumes stay."
        case .images:
            guard let image = snapshot.images.first(where: { $0.reference == id }) else { return }
            if snapshot.containers.contains(where: { $0.imageID == image.id }) {
                showError(DockerError(message: "\(id) is used by a container. Remove the container first."))
                return
            }
            alert.messageText = "Remove \(id)?"
            alert.informativeText = "The tag is removed. The image goes when no tag is left."
        case .networks:
            guard let network = snapshot.networks.first(where: { $0.id == id }) else { return }
            alert.messageText = "Remove network \(network.name)?"
            alert.informativeText = "Docker refuses while a container is attached."
        }
        alert.addButton(withTitle: force ? "Force Remove" : "Remove").hasDestructiveAction = true
        alert.addButton(withTitle: "Cancel")
        alert.beginSheetModal(for: window) { response in
            guard response == .alertFirstButtonReturn else { return }
            Task {
                do {
                    switch section {
                    case .containers: try await docker.removeContainer(id, force: force)
                    case .images: try await docker.removeImage(id)
                    case .networks: try await docker.removeNetwork(id)
                    }
                } catch {
                    self.showError(error)
                }
            }
        }
    }

    private var canRemoveTarget: Bool {
        guard let id = targetID else { return false }
        switch section {
        case .containers: return targetContainer != nil
        case .images: return snapshot.images.contains { $0.reference == id }
        case .networks: return snapshot.networks.contains { $0.id == id && !$0.isSystem }
        }
    }

    @objc func pullImage(_ sender: Any?) {
        guard let docker, let window, pulling == nil else { return }
        showSection(.images)
        let alert = NSAlert()
        alert.messageText = "Pull Image"
        alert.informativeText = "Without a tag or digest, the latest tag is pulled."
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 280, height: 22))
        field.placeholderString = "nginx:1.27"
        alert.accessoryView = field
        alert.addButton(withTitle: "Pull")
        alert.addButton(withTitle: "Cancel")
        alert.window.initialFirstResponder = field
        alert.beginSheetModal(for: window) { response in
            let reference = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
            guard response == .alertFirstButtonReturn, !reference.isEmpty else { return }
            self.pulling = reference
            self.updateChrome()
            Task {
                do {
                    try await docker.pull(reference)
                } catch {
                    self.showError(error)
                }
                self.pulling = nil
                self.updateChrome()
            }
        }
    }

    @objc func pruneImages(_ sender: Any?) {
        showSection(.images)
        pruneUnused(sender)
    }

    @objc func pruneNetworks(_ sender: Any?) {
        showSection(.networks)
        pruneUnused(sender)
    }

    @objc func pruneUnused(_ sender: Any?) {
        guard let docker, let window, section != .containers else { return }
        let images = section == .images
        let alert = NSAlert()
        alert.messageText = images ? "Remove unused images?" : "Remove unused networks?"
        alert.informativeText = images
            ? "Every image no container uses is removed, tagged ones too."
            : "Every network no container uses is removed."
        alert.addButton(withTitle: "Remove").hasDestructiveAction = true
        alert.addButton(withTitle: "Cancel")
        alert.beginSheetModal(for: window) { response in
            guard response == .alertFirstButtonReturn else { return }
            Task {
                do {
                    let message = images
                        ? "Removed unused images, reclaimed \(formatBytes(UInt64(max(0, try await docker.pruneImages()))))."
                        : "Removed \(try await docker.pruneNetworks()) unused networks."
                    self.showMessage(message, informative: "")
                } catch {
                    self.showError(error)
                }
            }
        }
    }

    func showError(_ error: Error) {
        showMessage("Docker returned an error", informative: error.localizedDescription)
    }

    private func showMessage(_ message: String, informative: String) {
        guard let window else { return }
        let alert = NSAlert()
        alert.messageText = message
        alert.informativeText = informative
        alert.beginSheetModal(for: window)
    }

    private func canPerform(_ action: Selector?) -> Bool {
        switch action {
        case #selector(startContainer): targetContainer.map { !$0.running } ?? false
        case #selector(stopContainer), #selector(openInTerminal): targetContainer?.running ?? false
        case #selector(restartContainer), #selector(toggleStart): targetContainer != nil
        case #selector(removeSelected): canRemoveTarget
        case #selector(pullImage): docker != nil && pulling == nil
        case #selector(pruneUnused), #selector(pruneImages), #selector(pruneNetworks): docker != nil
        case #selector(toggleOnlyRunning): section == .containers && openID == nil
        case #selector(toggleTimestamps): openContainer != nil
        default: true
        }
    }

    func validateToolbarItem(_ item: NSToolbarItem) -> Bool {
        canPerform(item.action)
    }

    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        canPerform(menuItem.action)
    }

    func selectionChanged() {
        window?.toolbar?.validateVisibleItems()
    }

    // MARK: Keys

    /// Esc and Return apply unless a text field is being edited, so they work with the
    /// list, the sidebar or a read-only log in focus.
    private func handleKey(_ event: NSEvent) -> Bool {
        guard let window, event.window === window, window.attachedSheet == nil else { return false }
        if let editor = window.firstResponder as? NSTextView, editor.isEditable { return false }
        let modifiers = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
            .subtracting([.capsLock, .numericPad, .function])
        let key = event.charactersIgnoringModifiers ?? ""
        if modifiers == .command, let digit = Int(key), (1...9).contains(digit) {
            switchRuntime(digit - 1)
            return true
        }
        guard modifiers.isEmpty || modifiers == .shift else { return false }
        switch (event.keyCode, key) {
        case (53, _):
            guard openID != nil else { return false }
            closeDetail()
        case (36, _), (76, _):
            guard openID == nil else { return false }
            if list.handleGroupKey(event.keyCode) { return true }
            guard let id = list.selectedID else { return false }
            open(id)
        case (123, _), (124, _):
            guard openID == nil, window.firstResponder === list.tableView else { return false }
            return list.handleGroupKey(event.keyCode)
        default:
            return false
        }
        return true
    }

    // MARK: Title and toolbar

    func updateChrome() {
        updateTitle()
        updateToolbar()
        window?.toolbar?.validateVisibleItems()
    }

    private func updateTitle() {
        guard let window else { return }
        var subtitle: [String] = []
        if let openID {
            switch section {
            case .containers:
                let container = snapshot.containers.first { $0.id == openID }
                window.title = container?.name ?? ""
                subtitle = [container?.image ?? "", container?.ports ?? "", container?.status ?? ""]
            case .images:
                let image = snapshot.images.first { $0.reference == openID }
                window.title = openID
                subtitle = [shortID(image?.id ?? ""), formatBytes(UInt64(max(0, image?.size ?? 0)))]
            case .networks:
                let network = snapshot.networks.first { $0.id == openID }
                window.title = network?.name ?? ""
                subtitle = [network?.driver ?? "", network?.scope ?? ""]
            }
        } else if docker == nil {
            window.title = "Dockwatch"
            let chosen = runtimes.first { $0.socket == chosenSocket }
            subtitle = [chosen.map { "\($0.name) is not running" } ?? "No running Docker runtime found"]
        } else {
            window.title = section.title
            switch section {
            case .containers:
                let running = snapshot.containers.filter(\.running).count
                subtitle = ["\(snapshot.containers.count) containers, \(running) running"]
            case .images:
                subtitle = ["\(snapshot.images.count) images"]
            case .networks:
                subtitle = ["\(snapshot.networks.count) networks"]
            }
        }
        if let pulling {
            subtitle.append("Pulling \(pulling)…")
        }
        window.subtitle = subtitle.filter { !$0.isEmpty }.joined(separator: " · ")
    }

    /// Actions sit at the trailing edge in every mode, so switching modes never moves them.
    private var toolbarIdentifiers: [NSToolbarItem.Identifier] {
        var identifiers: [NSToolbarItem.Identifier] = [.sidebarTrackingSeparator]
        if openID != nil {
            identifiers += [.back, .flexibleSpace]
            if section == .containers {
                identifiers += [.panes, .flexibleSpace]
                if detail.pane == .logs { identifiers.append(.logFilter) }
                return identifiers + [.terminal, .start, .stop, .restart, .remove]
            }
            return identifiers + [.remove]
        }
        identifiers.append(.flexibleSpace)
        switch section {
        case .containers: return identifiers + [.runningFilter, .search, .terminal, .start, .stop, .restart, .remove]
        // Every section has a filter in the same slot, so NSToolbar morphs one into the other; it does not
        // animate items that only move, so a slot left empty would make Search jump.
        case .images: return identifiers + [.unusedFilter, .search, .actionSpacer, .pull, .remove, .prune]
        case .networks: return identifiers + [.unusedFilter, .search, .remove, .prune]
        }
    }

    /// Every mode's items in one order; a mode change hides the unused ones, which NSToolbar animates.
    private let allToolbarIdentifiers: [NSToolbarItem.Identifier] = [
        .sidebarTrackingSeparator, .back, .flexibleSpace, .panes, .flexibleSpace, .runningFilter, .unusedFilter, .search, .logFilter, .actionSpacer,
        .terminal, .start, .stop, .restart, .pull, .remove, .prune,
    ]

    private func updateToolbar() {
        guard let toolbar = window?.toolbar else { return }
        let wanted = Set(toolbarIdentifiers)
        NSAnimationContext.runAnimationGroup { context in
            context.duration = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion ? 0 : 0.25
            context.allowsImplicitAnimation = true
            for item in toolbar.items {
                item.isHidden = !wanted.contains(item.itemIdentifier)
            }
        }
        (toolbarItems[.panes] as? NSToolbarItemGroup)?.selectedIndex = detail.pane.rawValue
        (toolbarItems[.runningFilter] as? NSToolbarItemGroup)?.selectedIndex = onlyRunning ? 1 : 0
        (toolbarItems[.unusedFilter] as? NSToolbarItemGroup)?.selectedIndex = onlyUnused ? 1 : 0
    }

    func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        allToolbarIdentifiers
    }

    func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        allToolbarIdentifiers
    }

    func toolbar(
        _ toolbar: NSToolbar, itemForItemIdentifier identifier: NSToolbarItem.Identifier,
        willBeInsertedIntoToolbar flag: Bool
    ) -> NSToolbarItem? {
        if let item = toolbarItems[identifier] { return item }
        let item: NSToolbarItem
        switch identifier {
        case .back:
            item = button(identifier, "Back", "chevron.left", #selector(goBack), key: "esc")
            item.isNavigational = true
        case .terminal: item = button(identifier, "Open in Terminal", "apple.terminal", #selector(openInTerminal), key: "⌘T")
        case .start: item = button(identifier, "Start", "play.fill", #selector(startContainer), key: nil)
        case .stop: item = button(identifier, "Stop", "stop.fill", #selector(stopContainer), key: nil)
        case .restart: item = button(identifier, "Restart", "arrow.clockwise", #selector(restartContainer), key: nil)
        case .remove: item = button(identifier, "Remove", "trash", #selector(removeSelected), key: nil)
        case .pull: item = button(identifier, "Pull Image", "arrow.down.circle", #selector(pullImage), key: nil)
        case .prune: item = button(identifier, "Remove Unused", "xmark.bin", #selector(pruneUnused), key: nil)
        case .panes:
            let titles = Pane.allCases.map(\.title)
            item = NSToolbarItemGroup(
                itemIdentifier: identifier, titles: titles, selectionMode: .selectOne, labels: titles,
                target: self, action: #selector(paneChanged)
            )
        case .runningFilter:
            item = NSToolbarItemGroup(
                itemIdentifier: identifier, titles: ["All", "Running"], selectionMode: .selectOne,
                labels: ["All", "Running"], target: self, action: #selector(runningFilterChanged)
            )
            item.toolTip = "Show only running containers"
        case .unusedFilter:
            item = NSToolbarItemGroup(
                itemIdentifier: identifier, titles: ["All", "Unused"], selectionMode: .selectOne,
                labels: ["All", "Unused"], target: self, action: #selector(unusedFilterChanged)
            )
            item.toolTip = "Show only what no container uses"
        case .search, .logFilter:
            let search = NSSearchToolbarItem(itemIdentifier: identifier)
            let isLogFilter = identifier == .logFilter
            search.searchField.placeholderString = isLogFilter ? "Filter logs" : "Search"
            search.preferredWidthForSearchField = 220
            search.searchField.target = self
            search.searchField.action = isLogFilter ? #selector(logFilterChanged) : #selector(searchChanged)
            item = search
        case .actionSpacer:
            item = NSToolbarItem(itemIdentifier: identifier)
            let view = NSView()
            view.widthAnchor.constraint(equalToConstant: 72).isActive = true
            view.heightAnchor.constraint(equalToConstant: 20).isActive = true
            item.view = view
            item.isBordered = false
        default:
            return nil
        }
        toolbarItems[identifier] = item
        return item
    }

    private func button(
        _ identifier: NSToolbarItem.Identifier, _ label: String, _ symbol: String, _ action: Selector, key: String?
    ) -> NSToolbarItem {
        let item = NSToolbarItem(itemIdentifier: identifier)
        item.label = label
        item.toolTip = key.map { "\(label) (\($0))" } ?? label
        item.image = NSImage(systemSymbolName: symbol, accessibilityDescription: label)
        item.isBordered = true
        item.target = self
        item.action = action
        return item
    }
}
