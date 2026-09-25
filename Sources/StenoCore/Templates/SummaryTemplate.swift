import Foundation

/// A fixed summary template: data, not code. Section ids and headings are the
/// core foundation's; `context` and every section's `instructions` are prompt
/// text the LLM workstream edits in `Resources/Templates/*.json`.
public struct SummaryTemplate: Codable, Sendable, Equatable, Hashable, Identifiable {
  public var id: String
  public var displayName: String
  public var description: String
  public var context: String
  public var sections: [TemplateSection]

  public init(
    id: String, displayName: String, description: String, context: String,
    sections: [TemplateSection]
  ) {
    self.id = id
    self.displayName = displayName
    self.description = description
    self.context = context
    self.sections = sections
  }

  public static let defaultID = "default"

  public func section(id: String) -> TemplateSection? {
    sections.first { $0.id == id }
  }
}

public struct TemplateSection: Codable, Sendable, Equatable, Hashable, Identifiable {
  public var id: String
  public var heading: String
  public var instructions: String
  public var required: Bool

  public init(id: String, heading: String, instructions: String, required: Bool) {
    self.id = id
    self.heading = heading
    self.instructions = instructions
    self.required = required
  }
}
