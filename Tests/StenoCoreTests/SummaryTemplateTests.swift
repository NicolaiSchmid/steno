import Foundation
import Testing

@testable import StenoCore

@Suite struct SummaryTemplateTests {
  @Test func bundledIDsAreTheFourInMenuOrder() {
    #expect(
      SummaryTemplate.bundled.map(\.id) == [
        "default", "customer-discovery", "daily-standup", "interview",
      ])
    #expect(SummaryTemplate.bundled.map(\.id) == SummaryTemplate.bundledIDs)
    #expect(SummaryTemplate.bundled(id: "default")?.displayName == "Default")
    #expect(SummaryTemplate.bundled(id: "interview")?.displayName == "Interview")
    #expect(SummaryTemplate.bundled(id: "missing") == nil)
    #expect(SummaryTemplate.defaultID == "default")
  }

  @Test func everySectionIsComplete() {
    for template in SummaryTemplate.bundled {
      #expect(!template.displayName.isEmpty, "\(template.id) displayName")
      #expect(!template.description.isEmpty, "\(template.id) description")
      #expect(!template.context.isEmpty, "\(template.id) context")
      #expect(!template.sections.isEmpty, "\(template.id) sections")
      #expect(template.sections.first?.required == true, "\(template.id) first section required")
      let ids = template.sections.map(\.id)
      #expect(Set(ids).count == ids.count, "\(template.id) unique section ids")
      for section in template.sections {
        #expect(!section.id.isEmpty, "\(template.id) section id")
        #expect(!section.heading.isEmpty, "\(template.id)/\(section.id) heading")
        #expect(!section.instructions.isEmpty, "\(template.id)/\(section.id) instructions")
        #expect(section.id == section.id.lowercased(), "\(template.id)/\(section.id) lowercase id")
        #expect(!section.id.contains(" "), "\(template.id)/\(section.id) hyphenated id")
      }
    }
  }

  @Test func sectionIDsAreTheOnesTheLLMPlanNames() {
    #expect(
      SummaryTemplate.bundled(id: "default")?.sections.map(\.id)
        == ["executive-summary", "full-summary", "open-questions"])
    #expect(
      SummaryTemplate.bundled(id: "customer-discovery")?.sections.map(\.id) == [
        "customer-context", "problems-and-pain-points", "current-workflow-and-tools",
        "reactions-and-buying-signals", "objections-and-risks", "next-steps",
      ])
    #expect(
      SummaryTemplate.bundled(id: "daily-standup")?.sections.map(\.id) == [
        "progress-since-last-standup", "plans-until-next-standup", "blockers-and-help-needed",
        "announcements",
      ])
    #expect(
      SummaryTemplate.bundled(id: "interview")?.sections.map(\.id) == [
        "candidate-background", "role-fit-and-experience", "skills-assessment",
        "motivation-and-culture", "candidate-questions", "assessment-and-recommendation",
        "next-steps",
      ])
    #expect(
      SummaryTemplate.bundled(id: "default")?.section(id: "executive-summary")?.heading
        == "Executive Summary")
  }

  /// Ids and headings are the core foundation's; a diff here without a model
  /// change is a rejected PR. The LLM workstream's edits to `instructions`
  /// and `context` update these goldens in the same PR with a sentence why.
  @Test func bundledTemplatesMatchTheirGoldens() throws {
    for template in SummaryTemplate.bundled {
      try Snapshot.assert(try StenoJSON.encode(template), matches: "templates/\(template.id).json")
    }
  }

  @Test func loadingFromTheWrongBundleThrows() {
    #expect(throws: (any Error).self) {
      try SummaryTemplate.load(id: "default", from: Bundle(for: BundleMarker.self))
    }
  }

  private final class BundleMarker {}
}
