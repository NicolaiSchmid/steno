import StenoCore
import Testing

@testable import StenoSpeech

/// `StageRates.seeds` lives in core, which cannot see `SpeechEngineID`; this
/// is where the two are held together.
@Suite struct StageRateSeedTests {
  @Test func everyEngineHasATranscribeSeed() {
    for id in SpeechEngineID.allCases {
      let seed = StageRates.seeds.entries[StageRates.Key(.transcribe, id.rawValue)]
      #expect(seed != nil, Comment(rawValue: id.rawValue))
      #expect(seed?.samples == 0, Comment(rawValue: id.rawValue))
      #expect((seed?.secondsPerUnit ?? 0) > 0, Comment(rawValue: id.rawValue))
    }
  }
}
