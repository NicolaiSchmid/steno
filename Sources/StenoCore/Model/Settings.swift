import Foundation

/// The one settings type, persisted as one row per property in the `setting`
/// table. Unknown or missing properties fall back to their defaults, so a
/// property can be added without a migration. API keys never live here; see
/// `SecretStore`.
public struct Settings: Codable, Sendable, Equatable, Hashable {
  /// Where recordings live; the user picks it in onboarding.
  public var audioFolder: URL
  public var defaultRetention: AudioRetention
  public var inputDeviceUID: String?
  public var meetingDetectionEnabled: Bool
  public var speechEngineID: String
  /// Cosine similarity a speaker match needs; the 0.05 margin is
  /// `SpeakerMemory.match`'s constant.
  public var speakerMatchThreshold: Float
  /// nil means the speech module's default location.
  public var modelsDirectory: URL?
  public var llmBaseURL: URL?
  public var llmModel: String?
  public var llmContextTokens: Int
  public var defaultTemplateID: String
  public var launchAtLogin: Bool
  /// nil means the Obsidian destination is not configured.
  public var obsidian: ObsidianSettings?

  public init(
    audioFolder: URL = Settings.defaultAudioFolder,
    defaultRetention: AudioRetention = .keepDays(30),
    inputDeviceUID: String? = nil,
    meetingDetectionEnabled: Bool = true,
    speechEngineID: String = "parakeet-v3",
    speakerMatchThreshold: Float = 0.60,
    modelsDirectory: URL? = nil,
    llmBaseURL: URL? = nil,
    llmModel: String? = nil,
    llmContextTokens: Int = 32_000,
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
    self.llmBaseURL = llmBaseURL
    self.llmModel = llmModel
    self.llmContextTokens = llmContextTokens
    self.defaultTemplateID = defaultTemplateID
    self.launchAtLogin = launchAtLogin
    self.obsidian = obsidian
  }

  /// `~/Library/Application Support/Steno/Audio`, until the user picks a
  /// folder.
  public static var defaultAudioFolder: URL {
    StenoPaths.defaultSupportDirectory.appendingPathComponent("Audio", isDirectory: true)
  }

  public init(from decoder: any Decoder) throws {
    let defaults = Settings()
    let container = try decoder.container(keyedBy: CodingKeys.self)
    audioFolder =
      try container.decodeIfPresent(URL.self, forKey: .audioFolder) ?? defaults.audioFolder
    defaultRetention =
      try container.decodeIfPresent(AudioRetention.self, forKey: .defaultRetention)
      ?? defaults.defaultRetention
    inputDeviceUID = try container.decodeIfPresent(String.self, forKey: .inputDeviceUID)
    meetingDetectionEnabled =
      try container.decodeIfPresent(Bool.self, forKey: .meetingDetectionEnabled)
      ?? defaults.meetingDetectionEnabled
    speechEngineID =
      try container.decodeIfPresent(String.self, forKey: .speechEngineID) ?? defaults.speechEngineID
    speakerMatchThreshold =
      try container.decodeIfPresent(Float.self, forKey: .speakerMatchThreshold)
      ?? defaults.speakerMatchThreshold
    modelsDirectory = try container.decodeIfPresent(URL.self, forKey: .modelsDirectory)
    llmBaseURL = try container.decodeIfPresent(URL.self, forKey: .llmBaseURL)
    llmModel = try container.decodeIfPresent(String.self, forKey: .llmModel)
    llmContextTokens =
      try container.decodeIfPresent(Int.self, forKey: .llmContextTokens)
      ?? defaults.llmContextTokens
    defaultTemplateID =
      try container.decodeIfPresent(String.self, forKey: .defaultTemplateID)
      ?? defaults.defaultTemplateID
    launchAtLogin =
      try container.decodeIfPresent(Bool.self, forKey: .launchAtLogin) ?? defaults.launchAtLogin
    obsidian = try container.decodeIfPresent(ObsidianSettings.self, forKey: .obsidian)
  }

  enum CodingKeys: String, CodingKey, CaseIterable {
    case audioFolder
    case defaultRetention
    case inputDeviceUID
    case meetingDetectionEnabled
    case speechEngineID
    case speakerMatchThreshold
    case modelsDirectory
    case llmBaseURL
    case llmModel
    case llmContextTokens
    case defaultTemplateID
    case launchAtLogin
    case obsidian
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
