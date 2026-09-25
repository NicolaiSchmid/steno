// swift-tools-version: 6.1

import PackageDescription

// Every target compiles in Swift 6 language mode with strict concurrency. The
// dependency direction is fixed by the v1 program: StenoCore depends on GRDB
// and Foundation only; every other module depends on StenoCore and on its own
// third-party packages; the `steno` CLI wires them together.

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
      resources: [.copy("Resources/Templates")]
    ),
    .target(name: "StenoAudio", dependencies: ["StenoCore"]),
    .target(name: "StenoSpeech", dependencies: ["StenoCore"]),
    .target(name: "StenoLLM", dependencies: ["StenoCore"]),
    .target(name: "StenoAdapters", dependencies: ["StenoCore"]),
    .target(name: "StenoHandover", dependencies: ["StenoCore"]),
    .executableTarget(
      name: "steno",
      dependencies: [
        "StenoCore",
        "StenoAdapters",
        .product(name: "ArgumentParser", package: "swift-argument-parser"),
      ]
    ),
    .testTarget(name: "StenoCoreTests", dependencies: ["StenoCore"]),
    .testTarget(name: "StenoAudioTests", dependencies: ["StenoAudio"]),
    .testTarget(name: "StenoSpeechTests", dependencies: ["StenoSpeech"]),
    .testTarget(name: "StenoLLMTests", dependencies: ["StenoLLM"]),
    .testTarget(name: "StenoAdaptersTests", dependencies: ["StenoAdapters"]),
    .testTarget(name: "StenoHandoverTests", dependencies: ["StenoHandover"]),
    .testTarget(name: "stenoTests", dependencies: ["StenoCore"]),
    .testTarget(name: "StenoEndToEndTests", dependencies: ["StenoCore", "StenoAdapters"]),
  ],
  swiftLanguageModes: [.v6]
)
