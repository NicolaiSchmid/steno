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

  /// The bundled template ids in menu order.
  public static let bundledIDs = ["default", "customer-discovery", "daily-standup", "interview"]

  /// The four fixed templates from `Resources/Templates/*.json`, in
  /// `bundledIDs` order. Loaded once; a missing or malformed file is a
  /// packaging error and stops the process with the file name.
  public static let bundled: [SummaryTemplate] = bundledIDs.map { id in
    do {
      return try load(id: id, from: .module)
    } catch {
      preconditionFailure("Summary template \(id).json failed to load: \(error)")
    }
  }

  public static func bundled(id: String) -> SummaryTemplate? {
    bundled.first { $0.id == id }
  }

  /// Reads `Templates/<id>.json` from `bundle`.
  public static func load(id: String, from bundle: Bundle) throws -> SummaryTemplate {
    guard let url = bundle.url(forResource: id, withExtension: "json", subdirectory: "Templates")
    else {
      throw CocoaError(.fileNoSuchFile, userInfo: [NSFilePathErrorKey: "Templates/\(id).json"])
    }
    let template = try StenoJSON.decode(SummaryTemplate.self, from: Data(contentsOf: url))
    guard template.id == id else {
      throw DecodingError.dataCorrupted(
        DecodingError.Context(
          codingPath: [], debugDescription: "Templates/\(id).json declares id \(template.id)"))
    }
    return template
  }

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
