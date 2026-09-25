import Foundation
import StenoCore
import Testing

@testable import StenoLLM

/// The template wording is prompt text: a golden diff here is a reviewed
/// prompt change and needs a sentence in the PR.
@Suite struct TemplateBlocksTests {
  @Test(arguments: SummaryTemplate.bundledIDs)
  func templateBlocksMatchTheirGolden(id: String) throws {
    let template = try #require(SummaryTemplate.bundled(id: id))
    let blocks = SummaryPromptBuilder(template: template).templateBlocks()
    try Snapshot.assert(blocks + "\n", matches: "llm/prompts/template-blocks-\(id).txt")
    #expect(blocks.contains("Template: \(template.displayName)"))
    #expect(blocks.contains(template.context))
    for section in template.sections {
      #expect(blocks.contains("- \(section.id) — \"\(section.heading)\""), "\(id)/\(section.id)")
      #expect(blocks.contains(section.instructions), "\(id)/\(section.id) instructions")
      #expect(!section.instructions.hasSuffix(" "))
      #expect(section.instructions.hasSuffix("."), "\(id)/\(section.id) ends with a full stop")
    }
    #expect(blocks.contains("(required)"))
  }

  @Test func sectionIDsAreStillCoresAndOnlyWordingChanged() {
    #expect(
      SummaryTemplate.bundled(id: "default")?.sections.map(\.id) == [
        "executive-summary", "full-summary", "open-questions",
      ])
    #expect(
      SummaryTemplate.bundled(id: "daily-standup")?.sections.map(\.heading) == [
        "Progress Since Last Standup", "Plans Until Next Standup", "Blockers and Help Needed",
        "Announcements",
      ])
    for template in SummaryTemplate.bundled {
      #expect(
        template.context.lowercased().contains("never invent")
          || template.context.lowercased().contains("never merge"),
        "\(template.id)")
    }
  }

  @Test func draftSchemaUsesTheTemplatesSectionIDsAsAnEnum() throws {
    let template = try #require(SummaryTemplate.bundled(id: "interview"))
    let builder = SummaryPromptBuilder(template: template)
    let sections = builder.draftSchema.jsonValue["properties"]?["sections"]?["items"]
    #expect(
      sections?["properties"]?["id"]?["enum"]
        == .array(template.sections.map { .string($0.id) }))
    #expect(JSONSchemaStrictTests.problems(in: builder.draftSchema.jsonValue) == [])
    #expect(JSONSchemaStrictTests.problems(in: builder.notesSchema.jsonValue) == [])
  }
}

@Suite struct OutputLanguageTests {
  @Test func resolvesTheMeetingLanguageAndFallsBackToEnglish() {
    #expect(OutputLanguage.resolve(meeting: "de") == "de")
    #expect(OutputLanguage.resolve(meeting: "de-CH") == "de-CH")
    #expect(OutputLanguage.resolve(meeting: nil) == "en")
    #expect(OutputLanguage.resolve(meeting: "") == "en")
  }

  @Test func promptNamesAreEnglishAndMachineIndependent() {
    #expect(OutputLanguage.promptName("de") == "German")
    #expect(OutputLanguage.promptName("de-AT") == "German")
    #expect(OutputLanguage.promptName("en-US") == "English")
    #expect(OutputLanguage.promptName("fr") == "French")
    #expect(OutputLanguage.promptName("zz-Zz") == "zz-Zz")
  }
}
