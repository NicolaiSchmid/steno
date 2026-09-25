import Foundation

/// Whether a participant is the user or somebody else.
public enum ParticipantRole: String, Codable, Sendable, Equatable, Hashable, CaseIterable {
  case me
  case them
}

/// Somebody in the meeting. Calendar attendees are `Participant` rows with
/// `role == .them` written by the app at recording start; there is no separate
/// attendee list.
public struct Participant: Codable, Sendable, Equatable, Hashable, Identifiable {
  public var id: UUID
  public var meetingID: UUID
  public var personID: UUID?
  public var displayName: String
  public var role: ParticipantRole
  public var email: String?

  public init(
    id: UUID,
    meetingID: UUID,
    personID: UUID? = nil,
    displayName: String,
    role: ParticipantRole,
    email: String? = nil
  ) {
    self.id = id
    self.meetingID = meetingID
    self.personID = personID
    self.displayName = displayName
    self.role = role
    self.email = email
  }
}

/// A known voice across meetings. `embedding` is the running L2-normalised
/// mean of `sampleCount` enrolments; it is persisted as a BLOB and never
/// written to JSON.
public struct Person: Codable, Sendable, Equatable, Hashable, Identifiable {
  public var id: UUID
  public var displayName: String
  public var email: String?
  public var embedding: Embedding?
  public var sampleCount: Int
  public var createdAt: Date

  public init(
    id: UUID,
    displayName: String,
    email: String? = nil,
    embedding: Embedding? = nil,
    sampleCount: Int = 0,
    createdAt: Date
  ) {
    self.id = id
    self.displayName = displayName
    self.email = email
    self.embedding = embedding
    self.sampleCount = sampleCount
    self.createdAt = createdAt
  }

  private enum CodingKeys: String, CodingKey {
    case id
    case displayName
    case email
    case sampleCount
    case createdAt
  }
}

/// A speaker embedding: `Embedding.dimension` Float32 values, L2-normalised
/// when produced by the diarizer or `normalized()`.
public struct Embedding: Codable, Sendable, Equatable, Hashable {
  /// WeSpeaker embeddings from the FluidAudio diarizer.
  public static let dimension = 256

  public var values: [Float]

  public init(_ values: [Float]) {
    self.values = values
  }

  public init(from decoder: any Decoder) throws {
    values = try decoder.singleValueContainer().decode([Float].self)
  }

  public func encode(to encoder: any Encoder) throws {
    var container = encoder.singleValueContainer()
    try container.encode(values)
  }

  /// Little-endian Float32 bytes, the BLOB column format.
  public var data: Data {
    var data = Data(capacity: values.count * MemoryLayout<Float>.size)
    for value in values {
      withUnsafeBytes(of: value.bitPattern.littleEndian) { data.append(contentsOf: $0) }
    }
    return data
  }

  /// Reads little-endian Float32 bytes; nil when the length is not a multiple
  /// of four.
  public init?(data: Data) {
    let size = MemoryLayout<UInt32>.size
    guard data.count % size == 0 else { return nil }
    var values: [Float] = []
    values.reserveCapacity(data.count / size)
    var index = data.startIndex
    while index < data.endIndex {
      var bits: UInt32 = 0
      withUnsafeMutableBytes(of: &bits) { target in
        target.copyBytes(from: data[index..<index + size])
      }
      values.append(Float(bitPattern: UInt32(littleEndian: bits)))
      index += size
    }
    self.values = values
  }

  public var magnitude: Float {
    values.reduce(0) { $0 + $1 * $1 }.squareRoot()
  }

  /// The same direction with unit length; the zero vector stays zero.
  public func normalized() -> Embedding {
    let magnitude = self.magnitude
    guard magnitude > 0 else { return self }
    return Embedding(values.map { $0 / magnitude })
  }

  public func dot(_ other: Embedding) -> Float {
    zip(values, other.values).reduce(0) { $0 + $1.0 * $1.1 }
  }

  /// Cosine similarity in `-1...1`; zero when either vector is zero or the
  /// dimensions differ.
  public func cosineSimilarity(to other: Embedding) -> Float {
    guard values.count == other.values.count else { return 0 }
    let denominator = magnitude * other.magnitude
    guard denominator > 0 else { return 0 }
    return dot(other) / denominator
  }

  /// Weighted mean of two embeddings, renormalised. Used by the person and
  /// speaker merges and by `SpeakerMemory.enroll` implementations.
  public static func weightedMean(
    _ lhs: Embedding, weight lhsWeight: Float, _ rhs: Embedding, weight rhsWeight: Float
  ) -> Embedding {
    let total = lhsWeight + rhsWeight
    guard total > 0, lhs.values.count == rhs.values.count else { return lhs }
    let mixed = zip(lhs.values, rhs.values).map { left, right in
      (left * lhsWeight + right * rhsWeight) / total
    }
    return Embedding(mixed).normalized()
  }
}

/// How a diarization cluster maps to a person.
public enum SpeakerAssignment: Codable, Sendable, Equatable, Hashable {
  case unknown
  case suggested(personID: UUID, similarity: Float)
  case confirmed(personID: UUID)

  /// The person behind the assignment; nil for `.unknown`.
  public var personID: UUID? {
    switch self {
    case .unknown: nil
    case .suggested(let personID, _), .confirmed(let personID): personID
    }
  }

  public var isConfirmed: Bool {
    if case .confirmed = self { return true }
    return false
  }

  private struct Suggested: Codable {
    var personID: UUID
    var similarity: Float
  }

  private struct Confirmed: Codable {
    var personID: UUID
  }

  public init(from decoder: any Decoder) throws {
    let (name, payload) = try CaseCoding.decode(from: decoder)
    switch name {
    case "unknown": self = .unknown
    case "suggested":
      let suggested = try CaseCoding.decodePayload(Suggested.self, from: payload, case: name)
      self = .suggested(personID: suggested.personID, similarity: suggested.similarity)
    case "confirmed":
      let confirmed = try CaseCoding.decodePayload(Confirmed.self, from: payload, case: name)
      self = .confirmed(personID: confirmed.personID)
    default: throw CaseCoding.unknownCase(name, in: decoder)
    }
  }

  public func encode(to encoder: any Encoder) throws {
    switch self {
    case .unknown: try CaseCoding.encode("unknown", to: encoder)
    case .suggested(let personID, let similarity):
      try CaseCoding.encode(
        "suggested", payload: Suggested(personID: personID, similarity: similarity), to: encoder)
    case .confirmed(let personID):
      try CaseCoding.encode("confirmed", payload: Confirmed(personID: personID), to: encoder)
    }
  }
}

/// One diarization cluster inside a meeting. `clusterLabel` ("Speaker 1") is
/// stable across re-exports and is what the LLM sees; the summary renderer
/// replaces it with the current person name.
public struct Speaker: Codable, Sendable, Equatable, Hashable, Identifiable {
  public var id: UUID
  public var meetingID: UUID
  public var clusterLabel: String
  public var assignment: SpeakerAssignment
  public var embedding: Embedding?
  /// The diarizer-chosen range of the cluster's clearest speech.
  public var sampleClipRange: ClosedRange<TimeInterval>?
  /// 10 s 16 kHz WAV written by the pipeline, deleted on confirm.
  public var sampleClipURL: URL?
  /// Diarizer cluster quality in `0...1`, not the match score.
  public var clusterConfidence: Float

  public init(
    id: UUID,
    meetingID: UUID,
    clusterLabel: String,
    assignment: SpeakerAssignment = .unknown,
    embedding: Embedding? = nil,
    sampleClipRange: ClosedRange<TimeInterval>? = nil,
    sampleClipURL: URL? = nil,
    clusterConfidence: Float
  ) {
    self.id = id
    self.meetingID = meetingID
    self.clusterLabel = clusterLabel
    self.assignment = assignment
    self.embedding = embedding
    self.sampleClipRange = sampleClipRange
    self.sampleClipURL = sampleClipURL
    self.clusterConfidence = clusterConfidence
  }

  public var personID: UUID? { assignment.personID }

  private enum CodingKeys: String, CodingKey {
    case id
    case meetingID
    case clusterLabel
    case assignment
    case sampleClipRange
    case sampleClipURL
    case clusterConfidence
  }
}

/// A ranked candidate from `SpeakerMemory.candidates(for:limit:)`.
public struct SpeakerMatch: Sendable, Equatable {
  public var person: Person
  public var similarity: Float

  public init(person: Person, similarity: Float) {
    self.person = person
    self.similarity = similarity
  }
}
