Swift bindings for a3net-ffi
============================

This is the Swift package / xcframework consumer for the
A3Net C-ABI / uniffi surface. Mirrors iroh-ffi's layout:

    crates/a3net-ffi/bindings/swift/
    ├── Package.swift                # SPM manifest
    ├── AdnetFFI.podspec             # CocoaPods manifest
    ├── AdnetFFI.xcframework/        # Build artifact (gitignored)
    ├── AdnetFFI/
    │   ├── Sources/
    │   │   ├── AdnetFFISwift/       # Language-native facade
    │   │   └── AdnetFFISampleApp/   # SwiftUI iOS example
    │   └── Tests/
    │       └── AdnetFFITests/       # XCTest integration suite
    └── README.swift.md              # This file

Building
========

The Swift package is consumed one of two ways:

1. SPM (Swift Package Manager) — drop the path to the
   generated `AdnetFFI.xcframework` into your Xcode
   workspace, or refer to the GitHub release zip via
   `Package.swift`'s `releaseTag` / `releaseChecksum`.

2. CocoaPods — add `pod 'AdnetFFI', '~> 0.1'` to your
   `Podfile`.

To regenerate the bindings locally:

    make bindgen-swift          # uniffi → Swift
    make ios-xcframework        # compile + cross-compile + bundle

Pre-flight
==========

* macOS host with Xcode (15.x or later)
* `cargo install cbindgen --locked`
* `rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios`
