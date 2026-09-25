import Foundation
import Testing

@testable import StenoAdapters

@Suite struct SlugTests {
  @Test func transliteratesUmlautsAndStripsOtherDiacritics() {
    #expect(
      Slug.title("Produktstrategie: \"90/10\" & Roadmap für Q4")
        == "produktstrategie-90-10-roadmap-fuer-q4")
    #expect(Slug.title("Über Äpfel, Öl und Straße") == "ueber-aepfel-oel-und-strasse")
    #expect(Slug.title("Jérôme trifft Zoë in São Paulo") == "jerome-trifft-zoe-in-sao-paulo")
    #expect(Slug.title("a\u{308}") == "ae", "a decomposed umlaut folds like the precomposed one")
    #expect(Slug.title("Ñandú niño") == "nandu-nino")
  }

  @Test func collapsesSeparatorsAndTrimsHyphens() {
    #expect(Slug.title("  --Hello,   World!--  ") == "hello-world")
    #expect(Slug.title("a___b") == "a-b")
    #expect(Slug.title("Weekly / Sync #3") == "weekly-sync-3")
  }

  @Test func fallsBackToMeetingWhenNothingIsLeft() {
    #expect(Slug.title("") == "meeting")
    #expect(Slug.title("🎉🎉🎉") == "meeting")
    #expect(Slug.title("日本語") == "meeting")
    #expect(Slug.title("---") == "meeting")
  }

  @Test func cutsAtTheLastHyphenWithinSixtyCharacters() {
    let words = (1...20).map { "wort\($0)" }.joined(separator: " ")
    let slug = Slug.title(words)
    #expect(slug.count <= 60)
    #expect(!slug.hasSuffix("-"))
    #expect(slug == "wort1-wort2-wort3-wort4-wort5-wort6-wort7-wort8-wort9-wort10")
    let sixtyOne = String(repeating: "a", count: 61)
    #expect(Slug.title(sixtyOne) == String(repeating: "a", count: 60), "no hyphen: a hard cut")
    let boundary = String(repeating: "a", count: 60) + "-b"
    #expect(
      Slug.title(boundary) == String(repeating: "a", count: 60), "a cut on a hyphen keeps the word")
    #expect(Slug.title("abc-def", maxLength: 5) == "abc")
  }

  @Test func fileNameStripsForbiddenCharacters() {
    #expect(Slug.fileName("Anna Müller") == "Anna Müller")
    #expect(Slug.fileName("a/b\\c:d*e?f\"g<h>i|j#k^l[m]n") == "abcdefghijklmn")
    #expect(Slug.fileName("  spaced   out \t name  ") == "spaced out name")
    #expect(Slug.fileName("bell\u{07}char\u{7F}") == "bellchar")
    #expect(Slug.fileName("...dots...") == "dots")
    #expect(Slug.fileName("...") == "Unnamed")
    #expect(Slug.fileName("") == "Unnamed")
    #expect(Slug.fileName(".hidden.") == "hidden")
  }

  @Test func isIndependentOfTheProcessLocale() {
    // Slugging touches no locale API; this pins the property for the titles
    // that differ most between German and English collation and casing.
    let titles = ["Straße İstanbul", "ÄÖÜ äöü", "Ærø Œuvre", "İ ı I i"]
    for title in titles {
      let slug = Slug.title(title)
      #expect(slug.unicodeScalars.allSatisfy { $0.isASCII }, "\(title) -> \(slug)")
      #expect(slug == Slug.title(title))
    }
    #expect(Slug.title("Straße İstanbul") == "strasse-istanbul")
  }
}
