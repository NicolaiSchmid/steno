import Testing

@testable import StenoLLM

@Test func placeholderModuleCompiles() {
  #expect(String(describing: StenoLLM.self) == "StenoLLM")
}
