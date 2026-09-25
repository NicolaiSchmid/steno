import Testing

@testable import StenoSpeech

@Test func placeholderModuleCompiles() {
  #expect(String(describing: StenoSpeech.self) == "StenoSpeech")
}
