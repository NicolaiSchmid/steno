import Foundation

/// The structured summary the LLM returns for one template. Stored as JSON;
/// `SummaryMarkdown.render` turns it into Markdown with the current speaker
/// names at display and export time.
public struct SummaryDocument: Codable, Sendable, Equatable, Hashable {
  public var templateID: String
  @LanguageTag public var language: Locale.Language?
  public var sections: [SummarySection]

  public init(templateID: String, language: Locale.Language? = nil, sections: [SummarySection]) {
    self.templateID = templateID
    self.language = language
    self.sections = sections
  }

  /// Every bullet's lead and text, one line per bullet; written to
  /// `meeting.summaryText` for full-text search.
  public var plainText: String {
    sections
      .flatMap(\.bullets)
      .map { bullet in bullet.lead.isEmpty ? bullet.text : "\(bullet.lead): \(bullet.text)" }
      .joined(separator: "\n")
  }
}

/// One template section with its bullets. `id` and `heading` come from the
/// `TemplateSection`; the heading may be translated into the meeting language.
public struct SummarySection: Codable, Sendable, Equatable, Hashable {
  public var id: String
  public var heading: String
  public var bullets: [SummaryBullet]

  public init(id: String, heading: String, bullets: [SummaryBullet]) {
    self.id = id
    self.heading = heading
    self.bullets = bullets
  }
}

/// A bullet in Jamie's shape: `**lead**: text`. Speaker cluster labels in
/// either part are replaced by current names when rendered.
public struct SummaryBullet: Codable, Sendable, Equatable, Hashable {
  public var lead: String
  public var text: String

  public init(lead: String, text: String) {
    self.lead = lead
    self.text = text
  }
}
