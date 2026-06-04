// swift-tools-version:5.7
import PackageDescription

let package = Package(
    name: "webapp",
    targets: [
        .executableTarget(name: "webapp", path: "Sources/webapp")
    ]
)
