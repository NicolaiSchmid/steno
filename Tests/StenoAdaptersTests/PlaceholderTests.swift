import Testing

@testable import StenoAdapters

@Test func placeholderModuleCompiles() {
  #expect(String(describing: StenoAdapters.self) == "StenoAdapters")
}
