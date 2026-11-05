// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "Dockwatch",
    platforms: [.macOS(.v15)],
    targets: [
        .executableTarget(name: "Dockwatch", swiftSettings: [.swiftLanguageMode(.v5)]),
        .testTarget(name: "DockwatchTests", dependencies: ["Dockwatch"], swiftSettings: [.swiftLanguageMode(.v5)]),
    ]
)
