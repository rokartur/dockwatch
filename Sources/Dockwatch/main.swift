import AppKit

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var windowController: MainWindowController?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.mainMenu = makeMainMenu()
        let windowController = MainWindowController()
        windowController.showWindow(nil)
        self.windowController = windowController
        NSApp.activate()
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }
}

private func makeMainMenu() -> NSMenu {
    let mainMenu = NSMenu()
    func submenu(_ title: String, _ items: [NSMenuItem]) -> NSMenu {
        let menu = NSMenu(title: title)
        items.forEach(menu.addItem)
        let item = NSMenuItem()
        item.submenu = menu
        mainMenu.addItem(item)
        return menu
    }
    func item(_ title: String, _ action: Selector, _ key: String = "", _ modifiers: NSEvent.ModifierFlags = .command) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: key)
        item.keyEquivalentModifierMask = modifiers
        return item
    }
    let find = item("Find…", #selector(NSTextView.performTextFinderAction(_:)), "f")
    find.tag = NSTextFinder.Action.showFindInterface.rawValue

    _ = submenu("Dockwatch", [
        item("About Dockwatch", #selector(NSApplication.orderFrontStandardAboutPanel(_:))),
        .separator(),
        item("Hide Dockwatch", #selector(NSApplication.hide(_:)), "h"),
        item("Hide Others", #selector(NSApplication.hideOtherApplications(_:)), "h", [.command, .option]),
        item("Show All", #selector(NSApplication.unhideAllApplications(_:))),
        .separator(),
        item("Quit Dockwatch", #selector(NSApplication.terminate(_:)), "q"),
    ])
    _ = submenu("Edit", [
        item("Undo", Selector(("undo:")), "z"),
        item("Redo", Selector(("redo:")), "z", [.command, .shift]),
        .separator(),
        item("Cut", #selector(NSText.cut(_:)), "x"),
        item("Copy", #selector(NSText.copy(_:)), "c"),
        item("Paste", #selector(NSText.paste(_:)), "v"),
        item("Select All", #selector(NSText.selectAll(_:)), "a"),
        .separator(),
        find,
    ])
    _ = submenu("View", [
        item("Containers", #selector(MainWindowController.showContainers(_:))),
        item("Images", #selector(MainWindowController.showImages(_:))),
        item("Networks", #selector(MainWindowController.showNetworks(_:))),
        .separator(),
        item("Command Palette", #selector(MainWindowController.showPalette(_:)), "k"),
        item("Rescan Runtimes", #selector(MainWindowController.rescan(_:)), "r"),
        .separator(),
        item("Enter Full Screen", #selector(NSWindow.toggleFullScreen(_:)), "f", [.command, .control]),
    ])
    NSApp.windowsMenu = submenu("Window", [
        item("Minimize", #selector(NSWindow.performMiniaturize(_:)), "m"),
        item("Zoom", #selector(NSWindow.performZoom(_:))),
        .separator(),
        item("Close", #selector(NSWindow.performClose(_:)), "w"),
    ])
    return mainMenu
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.run()
