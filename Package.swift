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
    // StenoSpeech only. FluidAudio moves fast and has broken source
    // compatibility inside minor releases before, so it is pinned to one
    // minor; WhisperKit follows semantic versioning.
    .package(
      url: "https://github.com/FluidInference/FluidAudio.git", .upToNextMinor(from: "0.17.4")),
    .package(url: "https://github.com/argmaxinc/argmax-oss-swift.git", from: "1.1.0"),
    // StenoHandover only.
    .package(url: "https://github.com/apple/swift-nio.git", from: "2.80.0"),
    .package(url: "https://github.com/apple/swift-nio-transport-services.git", from: "1.23.0"),
    .package(url: "https://github.com/apple/swift-certificates.git", from: "1.10.0"),
    .package(url: "https://github.com/apple/swift-crypto.git", from: "3.0.0"),
    .package(url: "https://github.com/apple/swift-asn1.git", from: "1.0.0"),
  ],
  targets: [
    .target(
      name: "StenoCore",
      dependencies: [.product(name: "GRDB", package: "GRDB.swift")],
      resources: [.copy("Resources/Templates")]
    ),
    .target(name: "StenoAudio", dependencies: ["StenoCore"]),
    .target(
      name: "StenoSpeech",
      dependencies: [
        "StenoCore",
        .product(name: "FluidAudio", package: "FluidAudio"),
        .product(name: "WhisperKit", package: "argmax-oss-swift"),
      ]
    ),
    .target(name: "StenoLLM", dependencies: ["StenoCore"]),
    .target(name: "StenoAdapters", dependencies: ["StenoCore"]),
    .target(
      name: "StenoHandover",
      dependencies: [
        "StenoCore",
        .product(name: "NIOCore", package: "swift-nio"),
        .product(name: "NIOPosix", package: "swift-nio"),
        .product(name: "NIOHTTP1", package: "swift-nio"),
        .product(name: "NIOTransportServices", package: "swift-nio-transport-services"),
        .product(name: "X509", package: "swift-certificates"),
        .product(name: "Crypto", package: "swift-crypto"),
        .product(name: "SwiftASN1", package: "swift-asn1"),
      ]
    ),
    .executableTarget(
      name: "steno",
      dependencies: [
        "StenoCore",
        "StenoSpeech",
        "StenoAdapters",
        "StenoHandover",
        .product(name: "ArgumentParser", package: "swift-argument-parser"),
      ]
    ),
    .testTarget(name: "StenoCoreTests", dependencies: ["StenoCore"]),
    .testTarget(name: "StenoAudioTests", dependencies: ["StenoAudio"]),
    .testTarget(name: "StenoSpeechTests", dependencies: ["StenoSpeech"]),
    .testTarget(name: "StenoLLMTests", dependencies: ["StenoLLM"]),
    // GRDB only to corrupt a settings row in the coordinator tests.
    .testTarget(
      name: "StenoAdaptersTests",
      dependencies: ["StenoAdapters", .product(name: "GRDB", package: "GRDB.swift")]),
    .testTarget(
      name: "StenoHandoverTests",
      dependencies: [
        "StenoHandover",
        .product(name: "X509", package: "swift-certificates"),
        .product(name: "Crypto", package: "swift-crypto"),
        .product(name: "NIOCore", package: "swift-nio"),
        .product(name: "NIOPosix", package: "swift-nio"),
        .product(name: "NIOTransportServices", package: "swift-nio-transport-services"),
      ],
      // The symlink into the iOS module compiles the phone's pin check
      // verbatim; it imports CryptoKit and Security, which Linux lacks.
      exclude: linuxOnlyExclusions(["Support/PinnedTrustEvaluator.swift"])
    ),
    .testTarget(name: "stenoTests", dependencies: ["StenoCore"]),
    .testTarget(
      name: "StenoEndToEndTests",
      dependencies: ["StenoCore", "StenoSpeech", "StenoAdapters", "StenoHandover"],
      // The pinned handover client compiles the phone's evaluator (symlink);
      // it needs CryptoKit and Security, absent on Linux.
      exclude: linuxOnlyExclusions(["Support/PinnedTrustEvaluator.swift"])
    ),
  ],
  swiftLanguageModes: [.v6]
)

/// Files left out of a target when the manifest is evaluated on Linux, where
/// the Apple-only frameworks they import do not exist.
func linuxOnlyExclusions(_ paths: [String]) -> [String] {
  #if os(Linux)
    return paths
  #else
    return []
  #endif
}
