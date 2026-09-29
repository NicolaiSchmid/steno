import ArgumentParser
import Foundation
import StenoCore
import StenoSpeech

/// `steno dev diarize-sweep <wav>... [--thresholds 0.6 0.8 1.0] [--max-speakers n]
/// [--no-refinement] [--out report.json]`: runs the FluidAudio diarizer over
/// 16 kHz mono WAV files at several clustering thresholds and prints, per
/// file and threshold, the speaker count, each cluster's speech time and
/// the cosine range between the cluster embeddings. `--no-refinement`
/// reports the clusters as the mapping produced them, so the pass's effect
/// can be read off two runs. `--out` writes the same data with every cluster
/// embedding as JSON. This is the tool `.plans/2026-09-29-speaker-calibration.md`
/// was measured with (the step 6 calibration that
/// `.plans/2026-09-25-speech-and-speakers.md` deferred to #34); recordings
/// stay outside the repository, and the report holds embeddings and
/// durations only.
struct DevDiarizeSweep: AsyncParsableCommand {
  static let configuration = CommandConfiguration(
    commandName: "diarize-sweep",
    abstract: "Diarize recordings at several clustering thresholds and report the clusters.")

  @Argument(help: "16 kHz mono WAV files, one lane each (system.wav, mixed.wav or mic.wav).")
  var files: [String]

  @Option(
    parsing: .upToNextOption,
    help:
      "FluidAudio clustering thresholds (Euclidean cut on unit embeddings, larger merges more).")
  var thresholds: [Double] = [0.6, 0.7, 0.8, 0.9, 1.0]

  @Option(name: .customLong("max-speakers"), help: "Cap the speaker count per file.")
  var maxSpeakers: Int?

  @Option(name: .customLong("out"), help: "Write the report with every cluster embedding as JSON.")
  var output: String?

  @Flag(inversion: .prefixedNo, help: "Run the refinement pass after the mapping.")
  var refinement = true

  @OptionGroup var models: DevModels.Options

  /// One diarization of one file at one threshold.
  struct Run: Codable {
    struct Cluster: Codable {
      var label: String
      var seconds: Double
      var confidence: Float
      /// Encodes as the flat array of values.
      var embedding: Embedding?
    }

    var file: String
    var threshold: Double
    var maxSpeakers: Int?
    var audioSeconds: Double
    var wallSeconds: Double
    var clusters: [Cluster]
  }

  func validate() throws {
    guard !files.isEmpty else { throw ValidationError("Give at least one WAV file.") }
    for file in files where !FileManager.default.fileExists(atPath: file) {
      throw ValidationError("No such file: \(file)")
    }
    guard !thresholds.isEmpty else { throw ValidationError("Give at least one threshold.") }
  }

  func run() async throws {
    let store = try await models.store()
    var runs: [Run] = []
    for file in files {
      let url = URL(fileURLWithPath: file)
      let buffer = try WAVAudioDecoder.read(url)
      let name = url.pathComponents.suffix(2).joined(separator: "/")
      let mode = refinement ? "refined" : "mapping only"
      print("## \(name) (\(Self.minutes(buffer.duration))), \(mode)")
      print("| Threshold | Speakers | Minutes per cluster | Cosine between clusters |")
      print("|---:|---:|---|---|")
      for threshold in thresholds {
        let diarizer = try makeDiarizer(
          models: store,
          config: FluidDiarizerConfig(
            clusteringThreshold: threshold, maxSpeakers: maxSpeakers,
            refinesClusters: refinement))
        let started = Date()
        let result = try await diarizer.diarize(buffer)
        let wall = Date().timeIntervalSince(started)
        let clusters = result.clusters.map { cluster in
          Run.Cluster(
            label: cluster.label,
            seconds: cluster.ranges.reduce(0) { $0 + $1.upperBound - $1.lowerBound },
            confidence: cluster.clusterConfidence,
            embedding: cluster.embedding)
        }
        .sorted { $0.seconds > $1.seconds }
        runs.append(
          Run(
            file: file, threshold: threshold, maxSpeakers: maxSpeakers,
            audioSeconds: buffer.duration, wallSeconds: wall, clusters: clusters))
        let sizes = clusters.map { String(format: "%.1f", $0.seconds / 60) }.joined(separator: " ")
        print(
          "| \(threshold) | \(clusters.count) | \(sizes) | \(Self.cosineRange(clusters)) |")
      }
      print()
    }
    if let output {
      let encoder = JSONEncoder()
      encoder.outputFormatting = [.sortedKeys]
      try encoder.encode(runs).write(to: URL(fileURLWithPath: output), options: .atomic)
      print("Report: \(output)")
    }
  }

  static func minutes(_ seconds: TimeInterval) -> String {
    String(format: "%.0f min", seconds / 60)
  }

  /// "min – max" cosine over every pair of cluster embeddings; "–" for fewer
  /// than two.
  static func cosineRange(_ clusters: [Run.Cluster]) -> String {
    let embeddings = clusters.compactMap(\.embedding)
    let cosines = embeddings.indices.flatMap { lhs in
      embeddings[(lhs + 1)...].map { embeddings[lhs].cosineSimilarity(to: $0) }
    }
    guard let low = cosines.min(), let high = cosines.max() else { return "–" }
    return String(format: "%.2f – %.2f", low, high)
  }
}
