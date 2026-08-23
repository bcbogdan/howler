// swift-tools-version: 5.10
import PackageDescription

let package = Package(
    name: "HowlerMac",
    platforms: [.macOS(.v13)],
    products: [.executable(name: "HowlerMac", targets: ["HowlerMac"])],
    targets: [
        .executableTarget(
            name: "HowlerMac",
            path: "Sources/HowlerMac",
            linkerSettings: [
                .unsafeFlags(
                    ["../../target/debug/libhowler_application_ffi.a"],
                    .when(configuration: .debug)
                ),
                .unsafeFlags(
                    ["../../target/release/libhowler_application_ffi.a"],
                    .when(configuration: .release)
                )
            ]
        ),
        .testTarget(name: "HowlerMacTests", dependencies: ["HowlerMac"], path: "Tests/HowlerMacTests")
    ]
)
