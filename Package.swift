// swift-tools-version: 6.1

import PackageDescription

// Every target compiles in Swift 6 language mode with strict concurrency. The
// dependency direction is fixed by the v1 program: StenoCore depends on GRDB
// and Foundation only; every other module depends on StenoCore and on its own
// third-party packages; the `steno` CLI wires them together.
//
// `InferSendableFromCaptures` (which GRDB recommends for shorthand closures) is
// already on in Swift 6 language mode; enabling it again only warns.
let swiftSettings: [SwiftSetting] = []

let package = Package(
  name: "steno",
  platforms: [.macOS(.v15)],
  products: [
    .library(name: "StenoCore", targets: ["StenoCore"]),
    .library(name: "StenoAudio", targets: ["StenoAudio"]),
    .library(name: "StenoSpeech", targets: ["StenoSpeech"]),
    .library(name: "StenoLLM", targets: ["StenoLLM"]),
    .library(name: "StenoAdapters", targets: ["StenoAdapters"]),
    .library(name: "StenoHandover", targets: ["StenoHandover"]),
    .executable(name: "steno", targets: ["steno"]),
  ],
  dependencies: [
    .package(url: "https://github.com/groue/GRDB.swift.git", from: "7.11.1"),
    .package(url: "https://github.com/apple/swift-argument-parser.git", from: "1.8.2"),
  ],
  targets: [
    .target(
      name: "StenoCore",
      dependencies: [.product(name: "GRDB", package: "GRDB.swift")],
      resources: [.copy("Resources/Templates")],
      swiftSettings: swiftSettings
    ),
    .target(name: "StenoAudio", dependencies: ["StenoCore"], swiftSettings: swiftSettings),
    .target(name: "StenoSpeech", dependencies: ["StenoCore"], swiftSettings: swiftSettings),
    .target(name: "StenoLLM", dependencies: ["StenoCore"], swiftSettings: swiftSettings),
    .target(name: "StenoAdapters", dependencies: ["StenoCore"], swiftSettings: swiftSettings),
    .target(name: "StenoHandover", dependencies: ["StenoCore"], swiftSettings: swiftSettings),
    .executableTarget(
      name: "steno",
      dependencies: [
        "StenoCore",
        .product(name: "ArgumentParser", package: "swift-argument-parser"),
      ],
      swiftSettings: swiftSettings
    ),
    .testTarget(name: "StenoCoreTests", dependencies: ["StenoCore"], swiftSettings: swiftSettings),
    .testTarget(
      name: "StenoAudioTests", dependencies: ["StenoAudio"], swiftSettings: swiftSettings),
    .testTarget(
      name: "StenoSpeechTests", dependencies: ["StenoSpeech"], swiftSettings: swiftSettings),
    .testTarget(name: "StenoLLMTests", dependencies: ["StenoLLM"], swiftSettings: swiftSettings),
    .testTarget(
      name: "StenoAdaptersTests", dependencies: ["StenoAdapters"], swiftSettings: swiftSettings),
    .testTarget(
      name: "StenoHandoverTests", dependencies: ["StenoHandover"], swiftSettings: swiftSettings),
    .testTarget(name: "stenoTests", dependencies: ["StenoCore"], swiftSettings: swiftSettings),
    .testTarget(
      name: "StenoEndToEndTests", dependencies: ["StenoCore"], swiftSettings: swiftSettings),
  ],
  swiftLanguageModes: [.v6]
)
