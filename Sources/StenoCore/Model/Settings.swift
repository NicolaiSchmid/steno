import Foundation

/// The one settings type, persisted by `SettingsStore` as one row per property
/// in the `setting` table. API keys never live here; see `SecretStore`.
public enum LLMProvider: String, Codable, Sendable, Equatable, Hashable, CaseIterable {
  /// Any OpenAI-compatible chat completions server, base URL plus API key.
  case endpoint
  /// OpenAI's Codex backend with the ChatGPT sign-in the Codex CLI stored.
  case codex
}

public struct Settings: Codable, Sendable, Equatable, Hashable {
  /// Where recordings live; the user changes it in Settings > Recording.
  public var audioFolder: URL
  /// `.keepForever` for a new install: deletion is irreversible and the
  /// Recording section shows the disk cost next to the choice. A stored row wins.
  public var defaultRetention: AudioRetention
  public var inputDeviceUID: String?
  public var meetingDetectionEnabled: Bool
  public var speechEngineID: String
  /// Cosine similarity a speaker match needs; the 0.05 margin is
  /// `SpeakerMemory.match`'s constant.
  public var speakerMatchThreshold: Float
  /// nil means the speech module's default location.
  public var modelsDirectory: URL?
  /// Which service writes the summaries: an OpenAI-compatible endpoint
  /// (`llmBaseURL`, `llmModel`, `llmContextTokens`) or the user's ChatGPT
  /// plan through the Codex sign-in on this Mac (`codexModel`,
  /// `codexContextTokens`, gated by `codexConfirmedAt`). Both keep their
  /// fields, so switching back loses nothing.
  public var llmProvider: LLMProvider
  public var llmBaseURL: URL?
  public var llmModel: String?
  public var llmContextTokens: Int
  /// The Codex model slug (`gpt-5.6-terra`); nil until one is picked.
  public var codexModel: String?
  public var codexContextTokens: Int
  /// When the user confirmed that Steno may use the Codex sign-in stored on
  /// this Mac. nil means never: no code path reads the credentials file.
  public var codexConfirmedAt: Date?
  public var defaultTemplateID: String
  public var launchAtLogin: Bool
  /// nil means the Obsidian destination is not configured.
  public var obsidian: ObsidianSettings?

  public init(
    audioFolder: URL = Settings.defaultAudioFolder,
    defaultRetention: AudioRetention = .keepForever,
    inputDeviceUID: String? = nil,
    meetingDetectionEnabled: Bool = true,
    speechEngineID: String = "parakeet-v3",
    speakerMatchThreshold: Float = 0.60,
    modelsDirectory: URL? = nil,
    llmProvider: LLMProvider = .endpoint,
    llmBaseURL: URL? = nil,
    llmModel: String? = nil,
    llmContextTokens: Int = 32_000,
    codexModel: String? = nil,
    codexContextTokens: Int = Settings.defaultCodexContextTokens,
    codexConfirmedAt: Date? = nil,
    defaultTemplateID: String = SummaryTemplate.defaultID,
    launchAtLogin: Bool = true,
    obsidian: ObsidianSettings? = nil
  ) {
    self.audioFolder = audioFolder
    self.defaultRetention = defaultRetention
    self.inputDeviceUID = inputDeviceUID
    self.meetingDetectionEnabled = meetingDetectionEnabled
    self.speechEngineID = speechEngineID
    self.speakerMatchThreshold = speakerMatchThreshold
    self.modelsDirectory = modelsDirectory
    self.llmProvider = llmProvider
    self.llmBaseURL = llmBaseURL
    self.llmModel = llmModel
    self.llmContextTokens = llmContextTokens
    self.codexModel = codexModel
    self.codexContextTokens = codexContextTokens
    self.codexConfirmedAt = codexConfirmedAt
    self.defaultTemplateID = defaultTemplateID
    self.launchAtLogin = launchAtLogin
    self.obsidian = obsidian
  }

  /// Until the model list has told us better: every Codex model on offer in
  /// September 2026 reads 272k, and a lower guess only costs smaller chunks.
  public static let defaultCodexContextTokens = 128_000

  /// `~/Library/Application Support/Steno/Audio`, until the user picks a
  /// folder.
  public static var defaultAudioFolder: URL {
    StenoPaths.defaultSupportDirectory.appendingPathComponent("Audio", isDirectory: true)
  }
}

/// The Obsidian folder destination's typed settings. A second destination
/// adds a second typed optional to `Settings`.
public struct ObsidianSettings: Codable, Sendable, Equatable, Hashable {
  /// Absolute path of the vault.
  public var vaultPath: String
  /// Folder inside the vault for per-person pages; nil disables them.
  public var peopleFolder: String?
  public var includeAudio: Bool
  /// Tag appended to every task line; nil for none.
  public var taskTag: String?

  public init(
    vaultPath: String, peopleFolder: String? = nil, includeAudio: Bool = false,
    taskTag: String? = nil
  ) {
    self.vaultPath = vaultPath
    self.peopleFolder = peopleFolder
    self.includeAudio = includeAudio
    self.taskTag = taskTag
  }
}
