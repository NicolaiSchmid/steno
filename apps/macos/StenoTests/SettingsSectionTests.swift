import StenoCore
import StenoSpeech
import XCTest

/// The sections, the deep link, the sidebar subtitles and the user-facing
/// copy helpers of the Settings window. The rule under test: no library
/// name, port, id, licence or raw error in anything the window shows.
@MainActor
final class SettingsSectionTests: XCTestCase {
  func testOpenSettingsSetsAndReturnsTheRequest() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let controller = AppController(environment: environment)
    XCTAssertNil(controller.requestedSettingsSection)
    XCTAssertEqual(controller.openSettings(.summaries), .summaries)
    XCTAssertEqual(controller.requestedSettingsSection, .summaries)
    controller.requestedSettingsSection = nil
    XCTAssertEqual(controller.openSettings(.export), .export)
  }

  func testSectionsAreNamedByOutcome() {
    XCTAssertEqual(
      SettingsSection.allCases.map(\.title),
      ["General", "Recording", "Transcription", "Summaries", "Export", "iPhone"])
    for section in SettingsSection.allCases {
      for word in ["LLM", "Sparkle", "Obsidian", "listener", "diariz", "token"] {
        XCTAssertFalse(section.title.contains(word), "\(section) title says \(word)")
      }
      XCTAssertFalse(section.purpose.isEmpty)
    }
  }

  func testOverviewSubtitles() {
    var settings = Settings()
    let empty = SettingsOverviewViewModel.subtitles(
      settings: settings, recordingReady: false, modelsInstalled: false, pairedCount: 0,
      handoverAvailable: false, updateOutcome: .notChecked, version: "0.9.0")
    XCTAssertEqual(empty[.general], "Steno 0.9.0")
    XCTAssertEqual(empty[.recording], "Permission needed")
    XCTAssertEqual(empty[.transcription], "Download needed")
    XCTAssertEqual(empty[.summaries], "Not set up")
    XCTAssertEqual(empty[.export], "Off")
    XCTAssertEqual(empty[.iphone], "Unavailable")

    settings.llmBaseURL = URL(string: "https://api.openai.com/v1")
    settings.llmModel = "gpt-4.1-mini"
    settings.obsidian = ObsidianSettings(vaultPath: "/Users/me/Notes/Work Vault")
    let configured = SettingsOverviewViewModel.subtitles(
      settings: settings, recordingReady: true, modelsInstalled: true, pairedCount: 2,
      handoverAvailable: true, updateOutcome: .available("0.9.1"), version: "0.9.0")
    XCTAssertEqual(configured[.general], "Update available: 0.9.1")
    XCTAssertEqual(configured[.recording], "Ready")
    XCTAssertEqual(configured[.transcription], "Ready")
    XCTAssertEqual(configured[.summaries], "OpenAI")
    XCTAssertEqual(configured[.export], "Work Vault")
    XCTAssertEqual(configured[.iphone], "2 iPhones paired")

    settings.llmBaseURL = URL(string: "http://10.0.0.5:8080/v1")
    let custom = SettingsOverviewViewModel.subtitles(
      settings: settings, recordingReady: true, modelsInstalled: true, pairedCount: 1,
      handoverAvailable: true, updateOutcome: .failed("x"), version: "0.9.0")
    XCTAssertEqual(custom[.summaries], "gpt-4.1-mini", "a custom server shows the model")
    XCTAssertEqual(custom[.iphone], "1 iPhone paired")
    XCTAssertEqual(custom[.general], "Update check failed")
  }

  func testOverviewRefreshReadsTheEnvironment() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let overview = SettingsOverviewViewModel(environment: environment)
    await overview.refresh()
    XCTAssertEqual(overview.subtitles[.recording], "Ready", "the preview grants everything")
    XCTAssertEqual(overview.subtitles[.summaries], "Not set up")
    XCTAssertEqual(overview.subtitles[.export], "Off")
    XCTAssertEqual(overview.subtitles[.iphone], "Unavailable", "no handover in the preview")
    XCTAssertTrue(overview.subtitles[.general]?.hasPrefix("Steno ") ?? false)
  }

  func testPresetsInferFromTheStoredAddress() {
    XCTAssertEqual(LLMPreset.infer(from: nil), .lmStudio, "a fresh install starts local")
    XCTAssertEqual(LLMPreset.infer(from: URL(string: "http://127.0.0.1:1234/v1")), .lmStudio)
    XCTAssertEqual(LLMPreset.infer(from: URL(string: "http://127.0.0.1:1234/v1/")), .lmStudio)
    XCTAssertEqual(LLMPreset.infer(from: URL(string: "http://127.0.0.1:11434/v1")), .ollama)
    XCTAssertEqual(LLMPreset.infer(from: URL(string: "https://openrouter.ai/api/v1")), .openRouter)
    XCTAssertEqual(LLMPreset.infer(from: URL(string: "HTTPS://API.OPENAI.COM/v1")), .openAI)
    XCTAssertEqual(LLMPreset.infer(from: URL(string: "https://api.anthropic.com/v1")), .anthropic)
    XCTAssertEqual(LLMPreset.infer(from: URL(string: "http://10.0.0.5:8080/v1")), .custom)
    for preset in LLMPreset.allCases where preset != .custom {
      XCTAssertEqual(LLMPreset.infer(from: preset.baseURL), preset)
    }
    XCTAssertNil(LLMPreset.custom.baseURL)
    XCTAssertFalse(LLMPreset.lmStudio.needsAPIKey)
    XCTAssertTrue(LLMPreset.openAI.needsAPIKey)
    XCTAssertFalse(LLMPreset.openAI.showsServerField)
    XCTAssertTrue(LLMPreset.custom.showsServerField)
  }

  func testRetentionCopy() {
    typealias Mode = AudioSettingsViewModel.RetentionMode
    XCTAssertEqual(Mode.allCases, [.keepForever, .keepDays, .deleteAfterProcessing])
    XCTAssertEqual(Mode.keepForever.title(days: 30), "Forever")
    XCTAssertEqual(Mode.keepDays.title(days: 14), "For 14 days")
    XCTAssertEqual(Mode.deleteAfterProcessing.title(days: 30), "Until processed, then delete")
    XCTAssertTrue(Mode.keepDays.footnote(days: 14).contains("14 days"))
    XCTAssertTrue(Mode.deleteAfterProcessing.footnote(days: 1).contains("stay"))
    XCTAssertEqual(
      AudioSettingsViewModel.FolderUsage.bytes(4_200_000_000).text.hasPrefix("Recordings use "),
      true)
    XCTAssertEqual(AudioSettingsViewModel.FolderUsage.measuring.text, "Measuring…")
    XCTAssertEqual(AudioSettingsViewModel.FolderUsage.unavailable.text, "Size unavailable")
  }

  func testTranscriptionCopyNamesNoModelRepository() {
    XCTAssertEqual(SpeechSettingsViewModel.componentTitle(.parakeetV3), "Speech recognition")
    XCTAssertEqual(
      SpeechSettingsViewModel.componentTitle(.whisperLargeV3Turbo), "Speech recognition")
    XCTAssertEqual(SpeechSettingsViewModel.componentTitle(.offlineDiarizer), "Speaker recognition")
    let parakeet = SpeechSettingsViewModel.engineTitle(.parakeetV3)
    XCTAssertTrue(parakeet.hasPrefix("Parakeet · fast · "), parakeet)
    XCTAssertTrue(parakeet.hasSuffix(" languages"), parakeet)
    let whisper = SpeechSettingsViewModel.engineTitle(.whisperKitLargeV3Turbo)
    XCTAssertTrue(whisper.hasPrefix("Whisper · slower · "), whisper)
    XCTAssertEqual(
      SpeechSettingsViewModel.engineTitle(.parakeetDE), "Parakeet (German) · 1 language")
    for engine in SpeechEngineID.allCases {
      let title = SpeechSettingsViewModel.engineTitle(engine)
      XCTAssertFalse(title.contains("/"), "\(title) looks like a repository")
      XCTAssertFalse(title.lowercased().contains("coreml"), title)
    }
  }
}
