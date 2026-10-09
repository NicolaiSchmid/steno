import Foundation
import StenoCore

// MARK: - app

/// Window-wide state: what the main window needs before any meeting loads.
public struct AppSnapshot: Codable, Sendable, Equatable {
  public struct SetupBanner: Codable, Sendable, Equatable {
    public var title: String
    public var body: String
    public var offersSummaries: Bool
    public var offersVault: Bool

    public init(title: String, body: String, offersSummaries: Bool, offersVault: Bool) {
      self.title = title
      self.body = body
      self.offersSummaries = offersSummaries
      self.offersVault = offersVault
    }
  }

  public struct Phone: Codable, Sendable, Equatable {
    public var name: String
    public var lastSyncAt: Date?
    public var isReachable: Bool

    public init(name: String, lastSyncAt: Date?, isReachable: Bool) {
      self.name = name
      self.lastSyncAt = lastSyncAt
      self.isReachable = isReachable
    }
  }

  public var version: String
  public var setupBanner: SetupBanner?
  public var phone: Phone?
  /// Deep links: set once, consumed by the page, then cleared by the host.
  public var requestedMeetingID: UUID?
  public var requestedSettingsSection: BridgeSettingsSection?

  public init(
    version: String, setupBanner: SetupBanner? = nil,
    phone: Phone? = nil, requestedMeetingID: UUID? = nil,
    requestedSettingsSection: BridgeSettingsSection? = nil
  ) {
    self.version = version
    self.setupBanner = setupBanner
    self.phone = phone
    self.requestedMeetingID = requestedMeetingID
    self.requestedSettingsSection = requestedSettingsSection
  }
}

// MARK: - recording

public struct RecordingSnapshot: Codable, Sendable, Equatable {
  public enum State: String, Codable, Sendable, CaseIterable {
    case idle
    case starting
    case recording
    case stopping
  }

  public struct Level: Codable, Sendable, Equatable {
    /// RMS in 0...1 per lane; the page draws the meter, at most 20 Hz.
    public var mic: Double
    public var system: Double

    public init(mic: Double, system: Double) {
      self.mic = mic
      self.system = system
    }
  }

  public struct AutoStop: Codable, Sendable, Equatable {
    public var remainingSeconds: Double
    public var totalSeconds: Double
    public var reason: String

    public init(remainingSeconds: Double, totalSeconds: Double, reason: String) {
      self.remainingSeconds = remainingSeconds
      self.totalSeconds = totalSeconds
      self.reason = reason
    }
  }

  public var state: State
  public var startedAt: Date?
  public var mode: BridgeCaptureMode?
  public var callApp: String?
  public var meetingID: UUID?
  public var level: Level?
  public var autoStop: AutoStop?
  public var deniedPermissions: [BridgePermissionKind]
  public var warning: String?
  public var error: String?

  public init(
    state: State, startedAt: Date? = nil, mode: BridgeCaptureMode? = nil, callApp: String? = nil,
    meetingID: UUID? = nil, level: Level? = nil, autoStop: AutoStop? = nil,
    deniedPermissions: [BridgePermissionKind] = [], warning: String? = nil, error: String? = nil
  ) {
    self.state = state
    self.startedAt = startedAt
    self.mode = mode
    self.callApp = callApp
    self.meetingID = meetingID
    self.level = level
    self.autoStop = autoStop
    self.deniedPermissions = deniedPermissions
    self.warning = warning
    self.error = error
  }
}

// MARK: - progress

public struct ProgressSnapshot: Codable, Sendable, Equatable {
  public struct Entry: Codable, Sendable, Equatable {
    public var meetingID: UUID
    public var stage: String
    public var title: String
    public var fraction: Double
    public var estimatedRemainingSeconds: Double?

    public init(
      meetingID: UUID, stage: String, title: String, fraction: Double,
      estimatedRemainingSeconds: Double?
    ) {
      self.meetingID = meetingID
      self.stage = stage
      self.title = title
      self.fraction = fraction
      self.estimatedRemainingSeconds = estimatedRemainingSeconds
    }
  }

  public var entries: [Entry]

  public init(entries: [Entry]) { self.entries = entries }
}

// MARK: - meetings.list

public enum BridgeListFilter: String, Codable, Sendable, CaseIterable {
  case all
  case processing
  case ready
  case failed
}

public struct SpeakerChip: Codable, Sendable, Equatable {
  public var id: UUID
  public var initial: String
  /// Index into the page's fixed people palette; stable per person.
  public var colorIndex: Int
  public var isConfirmed: Bool

  public init(id: UUID, initial: String, colorIndex: Int, isConfirmed: Bool) {
    self.id = id
    self.initial = initial
    self.colorIndex = colorIndex
    self.isConfirmed = isConfirmed
  }
}

public struct MeetingRow: Codable, Sendable, Equatable {
  public var id: UUID
  public var title: String
  public var startedAt: Date
  public var durationSeconds: Double
  public var source: BridgeMeetingSource
  public var state: BridgeMeetingState
  public var failureReason: String?
  public var preview: String?
  public var hasSummary: Bool
  public var speakers: [SpeakerChip]
  public var tags: [String]

  public init(
    id: UUID, title: String, startedAt: Date, durationSeconds: Double,
    source: BridgeMeetingSource, state: BridgeMeetingState, failureReason: String? = nil,
    preview: String?, hasSummary: Bool, speakers: [SpeakerChip], tags: [String]
  ) {
    self.id = id
    self.title = title
    self.startedAt = startedAt
    self.durationSeconds = durationSeconds
    self.source = source
    self.state = state
    self.failureReason = failureReason
    self.preview = preview
    self.hasSummary = hasSummary
    self.speakers = speakers
    self.tags = tags
  }
}

public struct MeetingsListSnapshot: Codable, Sendable, Equatable {
  public struct Counts: Codable, Sendable, Equatable {
    public var all: Int
    public var processing: Int
    public var ready: Int
    public var failed: Int

    public init(all: Int, processing: Int, ready: Int, failed: Int) {
      self.all = all
      self.processing = processing
      self.ready = ready
      self.failed = failed
    }
  }

  public struct Tag: Codable, Sendable, Equatable {
    public var name: String
    public var count: Int

    public init(name: String, count: Int) {
      self.name = name
      self.count = count
    }
  }

  public struct DayGroup: Codable, Sendable, Equatable {
    /// Calendar day in the user's zone, `YYYY-MM-DD`; the page formats labels.
    public var day: String
    public var meetings: [MeetingRow]

    public init(day: String, meetings: [MeetingRow]) {
      self.day = day
      self.meetings = meetings
    }
  }

  public var filter: BridgeListFilter
  public var tagFilter: String?
  public var query: String
  public var counts: Counts
  public var tags: [Tag]
  public var groups: [DayGroup]
  public var selection: UUID?
  public var error: String?

  public init(
    filter: BridgeListFilter, tagFilter: String? = nil, query: String, counts: Counts,
    tags: [Tag], groups: [DayGroup], selection: UUID? = nil, error: String? = nil
  ) {
    self.filter = filter
    self.tagFilter = tagFilter
    self.query = query
    self.counts = counts
    self.tags = tags
    self.groups = groups
    self.selection = selection
    self.error = error
  }
}

// MARK: - meeting.detail

public struct MeetingDetailSnapshot: Codable, Sendable, Equatable {
  public enum Tab: String, Codable, Sendable, CaseIterable {
    case summary
    case transcript
    case tasks
    case notes
  }

  public struct Retention: Codable, Sendable, Equatable {
    public enum Kind: String, Codable, Sendable, CaseIterable {
      case deleted
      case deletesOn
      case keptUntilExportSucceeds
      case keptProcessingFailed
      /// Rust only: the Swift host never sends it.
      case keptIncomplete
      case keptWhileProcessing
      case keptForever
    }

    public var kind: Kind
    public var deletesAt: Date?
    public var keepsAudio: Bool
    public var showsKeepToggle: Bool
    public var filesExist: Bool

    public init(
      kind: Kind, deletesAt: Date?, keepsAudio: Bool, showsKeepToggle: Bool, filesExist: Bool
    ) {
      self.kind = kind
      self.deletesAt = deletesAt
      self.keepsAudio = keepsAudio
      self.showsKeepToggle = showsKeepToggle
      self.filesExist = filesExist
    }
  }

  public struct Speaker: Codable, Sendable, Equatable {
    public typealias Assignment = SpeakerAssignment.Kind

    public var id: UUID
    public var clusterLabel: String
    public var displayName: String
    public var assignment: Assignment
    public var personID: UUID?
    /// The confirmed or suggested person's email, when known.
    public var email: String?
    public var suggestionName: String?
    public var colorIndex: Int
    public var hasClip: Bool
    public var isPlaying: Bool

    public init(
      id: UUID, clusterLabel: String, displayName: String, assignment: Assignment,
      personID: UUID? = nil, email: String? = nil, suggestionName: String? = nil, colorIndex: Int,
      hasClip: Bool, isPlaying: Bool
    ) {
      self.id = id
      self.clusterLabel = clusterLabel
      self.displayName = displayName
      self.assignment = assignment
      self.personID = personID
      self.email = email
      self.suggestionName = suggestionName
      self.colorIndex = colorIndex
      self.hasClip = hasClip
      self.isPlaying = isPlaying
    }
  }

  public struct Template: Codable, Sendable, Equatable {
    public var id: String
    public var name: String

    public init(id: String, name: String) {
      self.id = id
      self.name = name
    }
  }

  public struct SummarySection: Codable, Sendable, Equatable {
    public struct Bullet: Codable, Sendable, Equatable {
      public var lead: String
      public var text: String

      public init(lead: String, text: String) {
        self.lead = lead
        self.text = text
      }
    }

    public var id: String
    public var heading: String
    public var bullets: [Bullet]

    public init(id: String, heading: String, bullets: [Bullet]) {
      self.id = id
      self.heading = heading
      self.bullets = bullets
    }
  }

  public struct Turn: Codable, Sendable, Equatable {
    public var id: UUID
    public var speakerID: UUID?
    public var speakerName: String
    public var startSeconds: Double
    public var endSeconds: Double
    public var text: String

    public init(
      id: UUID, speakerID: UUID?, speakerName: String, startSeconds: Double, endSeconds: Double,
      text: String
    ) {
      self.id = id
      self.speakerID = speakerID
      self.speakerName = speakerName
      self.startSeconds = startSeconds
      self.endSeconds = endSeconds
      self.text = text
    }
  }

  public struct Task: Codable, Sendable, Equatable {
    public typealias Priority = TaskPriority

    public var id: UUID
    public var text: String
    public var assigneeName: String?
    public var assigneeColorIndex: Int?
    public var dueDate: Date?
    public var priority: Priority
    public var done: Bool

    public init(
      id: UUID, text: String, assigneeName: String?, assigneeColorIndex: Int?, dueDate: Date?,
      priority: Priority, done: Bool
    ) {
      self.id = id
      self.text = text
      self.assigneeName = assigneeName
      self.assigneeColorIndex = assigneeColorIndex
      self.dueDate = dueDate
      self.priority = priority
      self.done = done
    }
  }

  public struct Export: Codable, Sendable, Equatable {
    public enum Status: String, Codable, Sendable, CaseIterable {
      case notConfigured
      case pending
      case delivered
      case failed
    }

    public var status: Status
    public var message: String
    public var canReexport: Bool
    public var canReveal: Bool

    public init(status: Status, message: String, canReexport: Bool, canReveal: Bool) {
      self.status = status
      self.message = message
      self.canReexport = canReexport
      self.canReveal = canReveal
    }
  }

  public struct SummaryStatus: Codable, Sendable, Equatable {
    public enum Kind: String, Codable, Sendable, CaseIterable {
      case pending
      case present
      case skippedUnconfigured
      case skippedRunnable
    }

    public var kind: Kind
    public var title: String?
    public var body: String?
    public var actionTitle: String?

    public init(kind: Kind, title: String? = nil, body: String? = nil, actionTitle: String? = nil) {
      self.kind = kind
      self.title = title
      self.body = body
      self.actionTitle = actionTitle
    }
  }

  public var id: UUID
  public var title: String
  public var startedAt: Date
  public var durationSeconds: Double
  public var language: String?
  public var source: BridgeMeetingSource
  public var state: BridgeMeetingState
  public var failureReason: String?
  public var endReason: String?
  public var tags: [String]
  public var tab: Tab
  public var retention: Retention
  public var speakers: [Speaker]
  public var templates: [Template]
  public var templateID: String
  public var summaryStatus: SummaryStatus
  public var summary: [SummarySection]
  public var transcript: [Turn]
  public var tasks: [Task]
  public var decisions: [String]
  public var notes: String
  public var export: Export
  /// Whether the page shows "Process again". The Swift app sends `false`:
  /// it refuses the action.
  public var canProcessAgain: Bool
  public var canRerunSummary: Bool
  public var isBusy: Bool
  public var error: String?

  public init(
    id: UUID, title: String, startedAt: Date, durationSeconds: Double, language: String?,
    source: BridgeMeetingSource, state: BridgeMeetingState, failureReason: String? = nil,
    endReason: String? = nil, tags: [String], tab: Tab, retention: Retention, speakers: [Speaker],
    templates: [Template], templateID: String, summaryStatus: SummaryStatus,
    summary: [SummarySection], transcript: [Turn], tasks: [Task], decisions: [String],
    notes: String, export: Export, canProcessAgain: Bool, canRerunSummary: Bool, isBusy: Bool,
    error: String? = nil
  ) {
    self.id = id
    self.title = title
    self.startedAt = startedAt
    self.durationSeconds = durationSeconds
    self.language = language
    self.source = source
    self.state = state
    self.failureReason = failureReason
    self.endReason = endReason
    self.tags = tags
    self.tab = tab
    self.retention = retention
    self.speakers = speakers
    self.templates = templates
    self.templateID = templateID
    self.summaryStatus = summaryStatus
    self.summary = summary
    self.transcript = transcript
    self.tasks = tasks
    self.decisions = decisions
    self.notes = notes
    self.export = export
    self.canProcessAgain = canProcessAgain
    self.canRerunSummary = canRerunSummary
    self.isBusy = isBusy
    self.error = error
  }
}

// MARK: - onboarding

/// The onboarding window: page 1's permission steps and page 2's two setup
/// rows. The Summaries row carries the same `SummariesSettingsSnapshot` the
/// Settings page renders (its `subtitle` is empty here; the row has no
/// sidebar), so the consent card and the endpoint form are one component on
/// both pages; the vault row carries only what its chooser needs. Both are
/// nil when the window has no settings to write (a bare permissions run).
public struct OnboardingSnapshot: Codable, Sendable, Equatable {
  public enum Page: String, Codable, Sendable, CaseIterable {
    case permissions
    case setup
  }

  public struct PermissionStep: Codable, Sendable, Equatable {
    public var kind: BridgePermissionKind
    public var state: BridgePermissionState
    /// Required steps gate page 1's Done; optional ones can be skipped.
    public var isRequired: Bool
    public var isRequesting: Bool
    public var isSkipped: Bool

    public init(
      kind: BridgePermissionKind, state: BridgePermissionState, isRequired: Bool,
      isRequesting: Bool, isSkipped: Bool
    ) {
      self.kind = kind
      self.state = state
      self.isRequired = isRequired
      self.isRequesting = isRequesting
      self.isSkipped = isSkipped
    }
  }

  public struct SetupStep: Codable, Sendable, Equatable {
    public enum Kind: String, Codable, Sendable, CaseIterable {
      case summaries
      case vault
    }

    public enum State: String, Codable, Sendable, CaseIterable {
      case open
      case saved
      case skipped
    }

    public var kind: Kind
    public var state: State
    /// The collapsed row's line once saved ("Saved: <model> at <host>").
    public var savedLine: String?

    public init(kind: Kind, state: State, savedLine: String? = nil) {
      self.kind = kind
      self.state = state
      self.savedLine = savedLine
    }
  }

  /// The Obsidian vault row: the chosen folder (nil until chosen), why the
  /// last save was refused, and the store's error if saving failed.
  public struct Vault: Codable, Sendable, Equatable {
    public var path: String?
    public var name: String?
    public var validationMessage: String?
    public var error: String?
    public var errorDetails: String?

    public init(
      path: String? = nil, name: String? = nil, validationMessage: String? = nil,
      error: String? = nil, errorDetails: String? = nil
    ) {
      self.path = path
      self.name = name
      self.validationMessage = validationMessage
      self.error = error
      self.errorDetails = errorDetails
    }
  }

  public var page: Page
  public var permissions: [PermissionStep]
  /// Every required permission granted: page 1 offers Continue instead of Later.
  public var permissionsComplete: Bool
  public var setup: [SetupStep]
  public var canSaveSummaries: Bool
  public var summaries: SummariesSettingsSnapshot?
  public var vault: Vault?
  public var retentionSentence: String?
  /// Set once onboarding is over; the page closes the window.
  public var finished: Bool

  public init(
    page: Page, permissions: [PermissionStep], permissionsComplete: Bool, setup: [SetupStep],
    canSaveSummaries: Bool, summaries: SummariesSettingsSnapshot? = nil, vault: Vault? = nil,
    retentionSentence: String? = nil, finished: Bool
  ) {
    self.page = page
    self.permissions = permissions
    self.permissionsComplete = permissionsComplete
    self.setup = setup
    self.canSaveSummaries = canSaveSummaries
    self.summaries = summaries
    self.vault = vault
    self.retentionSentence = retentionSentence
    self.finished = finished
  }
}
