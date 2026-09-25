import Foundation

public enum TaskPriority: String, Codable, Sendable, Equatable, Hashable, CaseIterable {
  case low
  case normal
  case high
}

/// A task the LLM extracted from the meeting.
public struct MeetingTask: Codable, Sendable, Equatable, Hashable, Identifiable {
  public var id: UUID
  public var meetingID: UUID
  public var text: String
  public var assigneePersonID: UUID?
  public var assigneeName: String?
  public var priority: TaskPriority
  public var dueDate: Date?
  public var done: Bool

  public init(
    id: UUID,
    meetingID: UUID,
    text: String,
    assigneePersonID: UUID? = nil,
    assigneeName: String? = nil,
    priority: TaskPriority = .normal,
    dueDate: Date? = nil,
    done: Bool = false
  ) {
    self.id = id
    self.meetingID = meetingID
    self.text = text
    self.assigneePersonID = assigneePersonID
    self.assigneeName = assigneeName
    self.priority = priority
    self.dueDate = dueDate
    self.done = done
  }
}

/// A decision the LLM extracted from the meeting.
public struct Decision: Codable, Sendable, Equatable, Hashable, Identifiable {
  public var id: UUID
  public var meetingID: UUID
  public var text: String

  public init(id: UUID, meetingID: UUID, text: String) {
    self.id = id
    self.meetingID = meetingID
    self.text = text
  }
}
