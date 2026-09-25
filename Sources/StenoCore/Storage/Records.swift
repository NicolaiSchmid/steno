import Foundation
import GRDB

// Row types: one per table, private to Storage. Payload enums flatten into
// columns here so the canonical types stay clean for meeting.json.
//
// Column encodings: UUID as uppercase TEXT, Date as GRDB's UTC text, arrays
// and nested Codable values as JSON text through StenoJSON, embeddings as
// little-endian Float32 BLOBs, Locale.Language as its BCP-47 tag.

protocol StenoRecord: Codable, FetchableRecord, PersistableRecord {}

extension StenoRecord {
  static func databaseUUIDEncodingStrategy(for column: String) -> DatabaseUUIDEncodingStrategy {
    .uppercaseString
  }

  static func databaseJSONEncoder(for column: String) -> JSONEncoder {
    StenoJSON.columnEncoder()
  }

  static func databaseJSONDecoder(for column: String) -> JSONDecoder {
    StenoJSON.decoder()
  }
}

extension StenoJSON {
  /// The `StenoJSON` convention on one line, for JSON columns.
  static func columnEncoder() -> JSONEncoder {
    let columnEncoder = Self.encoder()
    columnEncoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
    return columnEncoder
  }
}

extension Embedding: DatabaseValueConvertible {
  public var databaseValue: DatabaseValue { data.databaseValue }

  public static func fromDatabaseValue(_ dbValue: DatabaseValue) -> Embedding? {
    guard let data = Data.fromDatabaseValue(dbValue) else { return nil }
    return Embedding(data: data)
  }
}

// MARK: - meeting

struct MeetingRow: StenoRecord {
  static let databaseTableName = "meeting"

  var id: UUID
  var title: String
  var startedAt: Date
  var duration: Double
  var language: String?
  var source: MeetingSource
  var calendarEventID: String?
  var tags: [String]
  var state: String
  var failureReason: String?
  var templateID: String
  var summary: SummaryDocument?
  var summaryText: String
  var scratchpad: String
  var llmUsage: LLMUsage?
  var createdAt: Date
  var updatedAt: Date

  enum Columns {
    static let id = Column(CodingKeys.id)
    static let startedAt = Column(CodingKeys.startedAt)
    static let state = Column(CodingKeys.state)
    static let updatedAt = Column(CodingKeys.updatedAt)
  }

  init(_ meeting: Meeting) {
    id = meeting.id
    title = meeting.title
    startedAt = meeting.startedAt
    duration = meeting.duration
    language = meeting.language?.stenoIdentifier
    source = meeting.source
    calendarEventID = meeting.calendarEventID
    tags = meeting.tags
    switch meeting.state {
    case .recording: state = "recording"
    case .queued: state = "queued"
    case .processing: state = "processing"
    case .ready: state = "ready"
    case .failed(let reason):
      state = "failed"
      failureReason = reason
    }
    templateID = meeting.templateID
    summary = meeting.summary
    summaryText = meeting.summary?.plainText ?? ""
    scratchpad = meeting.scratchpad
    llmUsage = meeting.llmUsage
    createdAt = meeting.createdAt
    updatedAt = meeting.updatedAt
  }

  var meeting: Meeting {
    let meetingState: MeetingState
    switch state {
    case "recording": meetingState = .recording
    case "queued": meetingState = .queued
    case "processing": meetingState = .processing
    case "ready": meetingState = .ready
    default: meetingState = .failed(reason: failureReason ?? "")
    }
    return Meeting(
      id: id,
      title: title,
      startedAt: startedAt,
      duration: duration,
      language: language.map(Locale.Language.init(stenoIdentifier:)),
      source: source,
      calendarEventID: calendarEventID,
      tags: tags,
      state: meetingState,
      templateID: templateID,
      summary: summary,
      scratchpad: scratchpad,
      llmUsage: llmUsage,
      createdAt: createdAt,
      updatedAt: updatedAt
    )
  }
}

// MARK: - person

struct PersonRow: StenoRecord {
  static let databaseTableName = "person"

  var id: UUID
  var displayName: String
  var email: String?
  var embedding: Embedding?
  var sampleCount: Int
  var createdAt: Date

  enum Columns {
    static let id = Column(CodingKeys.id)
    static let displayName = Column(CodingKeys.displayName)
  }

  init(_ person: Person) {
    id = person.id
    displayName = person.displayName
    email = person.email
    embedding = person.embedding
    sampleCount = person.sampleCount
    createdAt = person.createdAt
  }

  var person: Person {
    Person(
      id: id, displayName: displayName, email: email, embedding: embedding,
      sampleCount: sampleCount, createdAt: createdAt)
  }
}

// MARK: - participant

struct ParticipantRow: StenoRecord {
  static let databaseTableName = "participant"

  var id: UUID
  var meetingID: UUID
  var personID: UUID?
  var displayName: String
  var role: ParticipantRole
  var email: String?

  enum Columns {
    static let id = Column(CodingKeys.id)
    static let meetingID = Column(CodingKeys.meetingID)
    static let personID = Column(CodingKeys.personID)
    static let displayName = Column(CodingKeys.displayName)
  }

  init(_ participant: Participant) {
    id = participant.id
    meetingID = participant.meetingID
    personID = participant.personID
    displayName = participant.displayName
    role = participant.role
    email = participant.email
  }

  var participant: Participant {
    Participant(
      id: id, meetingID: meetingID, personID: personID, displayName: displayName, role: role,
      email: email)
  }
}

// MARK: - speaker

struct SpeakerRow: StenoRecord {
  static let databaseTableName = "speaker"

  var id: UUID
  var meetingID: UUID
  var clusterLabel: String
  var assignment: String
  var personID: UUID?
  var similarity: Float?
  var embedding: Embedding?
  var sampleClipStart: Double?
  var sampleClipEnd: Double?
  var sampleClipURL: URL?
  var clusterConfidence: Float

  enum Columns {
    static let id = Column(CodingKeys.id)
    static let meetingID = Column(CodingKeys.meetingID)
    static let clusterLabel = Column(CodingKeys.clusterLabel)
    static let assignment = Column(CodingKeys.assignment)
    static let personID = Column(CodingKeys.personID)
  }

  init(_ speaker: Speaker) {
    id = speaker.id
    meetingID = speaker.meetingID
    clusterLabel = speaker.clusterLabel
    switch speaker.assignment {
    case .unknown:
      assignment = "unknown"
    case .suggested(let person, let score):
      assignment = "suggested"
      personID = person
      similarity = score
    case .confirmed(let person):
      assignment = "confirmed"
      personID = person
    }
    embedding = speaker.embedding
    sampleClipStart = speaker.sampleClipRange?.lowerBound
    sampleClipEnd = speaker.sampleClipRange?.upperBound
    sampleClipURL = speaker.sampleClipURL
    clusterConfidence = speaker.clusterConfidence
  }

  var speaker: Speaker {
    let speakerAssignment: SpeakerAssignment
    switch (assignment, personID) {
    case ("suggested", let person?):
      speakerAssignment = .suggested(personID: person, similarity: similarity ?? 0)
    case ("confirmed", let person?):
      speakerAssignment = .confirmed(personID: person)
    default:
      speakerAssignment = .unknown
    }
    var range: ClosedRange<TimeInterval>?
    if let sampleClipStart, let sampleClipEnd, sampleClipStart <= sampleClipEnd {
      range = sampleClipStart...sampleClipEnd
    }
    return Speaker(
      id: id,
      meetingID: meetingID,
      clusterLabel: clusterLabel,
      assignment: speakerAssignment,
      embedding: embedding,
      sampleClipRange: range,
      sampleClipURL: sampleClipURL,
      clusterConfidence: clusterConfidence
    )
  }
}

// MARK: - transcriptSegment

struct TranscriptSegmentRow: StenoRecord {
  static let databaseTableName = "transcriptSegment"

  var id: UUID
  var meetingID: UUID
  var start: Double
  var end: Double
  var speakerID: UUID?
  var lane: AudioLane
  var text: String
  var rawText: String

  enum Columns {
    static let id = Column(CodingKeys.id)
    static let meetingID = Column(CodingKeys.meetingID)
    static let start = Column(CodingKeys.start)
    static let speakerID = Column(CodingKeys.speakerID)
  }

  init(_ segment: TranscriptSegment) {
    id = segment.id
    meetingID = segment.meetingID
    start = segment.start
    end = segment.end
    speakerID = segment.speakerID
    lane = segment.lane
    text = segment.text
    rawText = segment.rawText
  }

  var segment: TranscriptSegment {
    TranscriptSegment(
      id: id, meetingID: meetingID, start: start, end: end, speakerID: speakerID, lane: lane,
      text: text, rawText: rawText)
  }
}

// MARK: - meetingTask, decision

struct MeetingTaskRow: StenoRecord {
  static let databaseTableName = "meetingTask"

  var id: UUID
  var meetingID: UUID
  var text: String
  var assigneePersonID: UUID?
  var assigneeName: String?
  var priority: TaskPriority
  var dueDate: Date?
  var done: Bool

  enum Columns {
    static let meetingID = Column(CodingKeys.meetingID)
    static let assigneePersonID = Column(CodingKeys.assigneePersonID)
  }

  init(_ task: MeetingTask) {
    id = task.id
    meetingID = task.meetingID
    text = task.text
    assigneePersonID = task.assigneePersonID
    assigneeName = task.assigneeName
    priority = task.priority
    dueDate = task.dueDate
    done = task.done
  }

  var task: MeetingTask {
    MeetingTask(
      id: id, meetingID: meetingID, text: text, assigneePersonID: assigneePersonID,
      assigneeName: assigneeName, priority: priority, dueDate: dueDate, done: done)
  }
}

struct DecisionRow: StenoRecord {
  static let databaseTableName = "decision"

  var id: UUID
  var meetingID: UUID
  var text: String

  enum Columns {
    static let meetingID = Column(CodingKeys.meetingID)
  }

  init(_ decision: Decision) {
    id = decision.id
    meetingID = decision.meetingID
    text = decision.text
  }

  var decision: Decision { Decision(id: id, meetingID: meetingID, text: text) }
}

// MARK: - audioAsset

struct AudioAssetRow: StenoRecord {
  static let databaseTableName = "audioAsset"

  var id: UUID
  var meetingID: UUID
  var url: URL
  var format: AudioFormat
  var lanes: [AudioLane]
  var sidecars16k: [AudioLane: URL]
  var mixdownURL: URL?
  var retention: String
  var retentionDays: Int?
  var expiresAt: Date?

  enum Columns {
    static let id = Column(CodingKeys.id)
    static let meetingID = Column(CodingKeys.meetingID)
    static let expiresAt = Column(CodingKeys.expiresAt)
  }

  init(_ asset: AudioAsset) {
    id = asset.id
    meetingID = asset.meetingID
    url = asset.url
    format = asset.format
    lanes = asset.lanes
    sidecars16k = asset.sidecars16k
    mixdownURL = asset.mixdownURL
    switch asset.retention {
    case .deleteAfterProcessing: retention = "deleteAfterProcessing"
    case .keepForever: retention = "keepForever"
    case .keepDays(let days):
      retention = "keepDays"
      retentionDays = days
    }
    expiresAt = asset.expiresAt
  }

  var asset: AudioAsset {
    let assetRetention: AudioRetention
    switch retention {
    case "deleteAfterProcessing": assetRetention = .deleteAfterProcessing
    case "keepDays": assetRetention = .keepDays(retentionDays ?? 0)
    default: assetRetention = .keepForever
    }
    return AudioAsset(
      id: id,
      meetingID: meetingID,
      url: url,
      format: format,
      lanes: lanes,
      sidecars16k: sidecars16k,
      mixdownURL: mixdownURL,
      retention: assetRetention,
      expiresAt: expiresAt
    )
  }
}

// MARK: - delivery

struct DeliveryRow: StenoRecord {
  static let databaseTableName = "delivery"

  var id: UUID
  var meetingID: UUID
  var destinationID: String
  var status: String
  var failureMessage: String?
  var lastAttemptAt: Date?
  var receipt: DeliveryReceipt?

  enum Columns {
    static let meetingID = Column(CodingKeys.meetingID)
    static let destinationID = Column(CodingKeys.destinationID)
  }

  init(_ delivery: Delivery) {
    id = delivery.id
    meetingID = delivery.meetingID
    destinationID = delivery.destinationID
    switch delivery.status {
    case .pending: status = "pending"
    case .delivered: status = "delivered"
    case .failed(let message):
      status = "failed"
      failureMessage = message
    }
    lastAttemptAt = delivery.lastAttemptAt
    receipt = delivery.receipt
  }

  var delivery: Delivery {
    let deliveryStatus: DeliveryStatus
    switch status {
    case "pending": deliveryStatus = .pending
    case "delivered": deliveryStatus = .delivered
    default: deliveryStatus = .failed(failureMessage ?? "")
    }
    return Delivery(
      id: id, meetingID: meetingID, destinationID: destinationID, status: deliveryStatus,
      lastAttemptAt: lastAttemptAt, receipt: receipt)
  }
}

// MARK: - pairedDevice, handoverReceipt

struct PairedDeviceRow: StenoRecord {
  static let databaseTableName = "pairedDevice"

  var id: UUID
  var name: String
  var pairedAt: Date
  var lastSeenAt: Date?
  var tokenHash: Data

  enum Columns {
    static let id = Column(CodingKeys.id)
    static let pairedAt = Column(CodingKeys.pairedAt)
    static let tokenHash = Column(CodingKeys.tokenHash)
  }

  init(_ device: PairedDevice, tokenHash: Data) {
    id = device.id
    name = device.name
    pairedAt = device.pairedAt
    lastSeenAt = device.lastSeenAt
    self.tokenHash = tokenHash
  }

  var device: PairedDevice {
    PairedDevice(id: id, name: name, pairedAt: pairedAt, lastSeenAt: lastSeenAt)
  }
}

struct HandoverReceiptRow: StenoRecord {
  static let databaseTableName = "handoverReceipt"

  var recordingID: UUID
  var deviceID: UUID
  var state: String
  var meetingID: UUID?
  var failureMessage: String?
  var byteCount: Int64
  var sha256: Data
  var chunkSize: Int
  var receivedChunks: [Int]
  var createdAt: Date
  var updatedAt: Date

  enum Columns {
    static let recordingID = Column(CodingKeys.recordingID)
  }

  init(_ receipt: HandoverReceipt) {
    recordingID = receipt.recordingID
    deviceID = receipt.deviceID
    switch receipt.state {
    case .receiving: state = "receiving"
    case .verifying: state = "verifying"
    case .complete(let meeting):
      state = "complete"
      meetingID = meeting
    case .failed(let message):
      state = "failed"
      failureMessage = message
    }
    byteCount = receipt.byteCount
    sha256 = receipt.sha256
    chunkSize = receipt.chunkSize
    receivedChunks = receipt.receivedChunks
    createdAt = receipt.createdAt
    updatedAt = receipt.updatedAt
  }

  var receipt: HandoverReceipt {
    let receiptState: HandoverReceipt.State
    switch (state, meetingID) {
    case ("receiving", _): receiptState = .receiving
    case ("verifying", _): receiptState = .verifying
    case ("complete", let meeting?): receiptState = .complete(meetingID: meeting)
    default: receiptState = .failed(failureMessage ?? "")
    }
    return HandoverReceipt(
      recordingID: recordingID,
      deviceID: deviceID,
      state: receiptState,
      byteCount: byteCount,
      sha256: sha256,
      chunkSize: chunkSize,
      receivedChunks: receivedChunks,
      createdAt: createdAt,
      updatedAt: updatedAt
    )
  }
}

// MARK: - setting

struct SettingRow: StenoRecord {
  static let databaseTableName = "setting"

  var key: String
  var value: String

  enum Columns {
    static let key = Column(CodingKeys.key)
  }
}
