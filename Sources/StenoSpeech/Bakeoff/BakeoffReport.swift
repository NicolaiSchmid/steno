import Foundation
import StenoCore

/// One file transcribed by one engine.
public struct BakeoffRow: Codable, Sendable, Equatable {
  public var file: String
  public var engine: SpeechEngineID
  public var audioSeconds: Double
  public var wallSeconds: Double
  public var segmentCount: Int
  /// Against `<name>.ref.txt`; nil when there is no reference.
  public var wer: Double?
  public var languageFlips: Int
  public var dominantLanguage: LanguageTag?
  /// WER after the injected `TranscriptCleaner`; nil without cleanup or
  /// reference.
  public var cleanedWER: Double?
  /// Model requests the cleaner made for this file, retries included; nil
  /// when cleanup did not run.
  public var cleanupRequests: Int?

  public init(
    file: String, engine: SpeechEngineID, audioSeconds: Double, wallSeconds: Double,
    segmentCount: Int, wer: Double? = nil, languageFlips: Int,
    dominantLanguage: LanguageTag? = nil, cleanedWER: Double? = nil, cleanupRequests: Int? = nil
  ) {
    self.file = file
    self.engine = engine
    self.audioSeconds = audioSeconds
    self.wallSeconds = wallSeconds
    self.segmentCount = segmentCount
    self.wer = wer
    self.languageFlips = languageFlips
    self.dominantLanguage = dominantLanguage
    self.cleanedWER = cleanedWER
    self.cleanupRequests = cleanupRequests
  }

  /// Seconds of audio per second of wall clock, FluidAudio's `RTFx` (the
  /// inverse of the real-time factor: higher is faster). Reported, never
  /// asserted.
  public var rtfx: Double { audioSeconds / max(wallSeconds, 1e-9) }
}

/// Every row of one bake-off run plus its Markdown and JSON renderings.
public struct BakeoffReport: Codable, Sendable, Equatable {
  public var rows: [BakeoffRow]
  public var generatedAt: Date

  public init(rows: [BakeoffRow], generatedAt: Date = Date()) {
    self.rows = rows
    self.generatedAt = generatedAt
  }

  /// One table, files in order, engines in the order requested, plus a
  /// per-engine mean of the columns that can be averaged.
  public func markdown() -> String {
    var lines = [
      "# STT bake-off", "",
      "| File | Engine | Audio s | Wall s | RTFx | Segments | WER | WER (cleaned) | "
        + "Cleanup requests | Flips | Language |",
      "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---|",
    ]
    for row in rows {
      lines.append(
        "| \(row.file) | \(row.engine.rawValue) | \(format(row.audioSeconds)) | "
          + "\(format(row.wallSeconds)) | \(format(row.rtfx)) | \(row.segmentCount) | "
          + "\(percent(row.wer)) | \(percent(row.cleanedWER)) | "
          + "\(row.cleanupRequests.map(String.init) ?? "-") | \(row.languageFlips) | "
          + "\(row.dominantLanguage?.rawValue ?? "-") |")
    }
    let engines = rows.map(\.engine).reduce(into: [SpeechEngineID]()) {
      if !$0.contains($1) { $0.append($1) }
    }
    if !engines.isEmpty {
      lines.append("")
      lines.append(
        "| Engine | Files | Mean RTFx | Mean WER | Mean WER (cleaned) | Cleanup requests | Flips |")
      lines.append("|---|---:|---:|---:|---:|---:|---:|")
      for engine in engines {
        let own = rows.filter { $0.engine == engine }
        let wers = own.compactMap(\.wer)
        let cleaned = own.compactMap(\.cleanedWER)
        let requests = own.compactMap(\.cleanupRequests)
        lines.append(
          "| \(engine.rawValue) | \(own.count) | \(format(mean(own.map(\.rtfx)))) | "
            + "\(percent(wers.isEmpty ? nil : mean(wers))) | "
            + "\(percent(cleaned.isEmpty ? nil : mean(cleaned))) | "
            + "\(requests.isEmpty ? "-" : String(requests.reduce(0, +))) | "
            + "\(own.reduce(0) { $0 + $1.languageFlips }) |")
      }
    }
    lines.append("")
    return lines.joined(separator: "\n")
  }

  /// Pretty-printed, sorted keys, ISO 8601 dates: what `report.json` and
  /// `steno dev bakeoff --json` contain.
  public func json() throws -> Data {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
    encoder.dateEncodingStrategy = .iso8601
    return try encoder.encode(self)
  }

  private func mean(_ values: [Double]) -> Double {
    values.isEmpty ? 0 : values.reduce(0, +) / Double(values.count)
  }

  private func format(_ value: Double) -> String {
    String(format: "%.2f", value)
  }

  private func percent(_ value: Double?) -> String {
    guard let value else { return "-" }
    return String(format: "%.1f %%", value * 100)
  }
}
