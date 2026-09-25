import Testing

@testable import StenoAudio

@Test func placeholderModuleCompiles() {
  #expect(String(describing: StenoAudio.self) == "StenoAudio")
}
