import Testing

/// Polls `condition` every millisecond for at most five seconds; else fails
/// at the caller's line and throws, so the test stops there.
func until(
  _ condition: () async -> Bool, sourceLocation: SourceLocation = #_sourceLocation
) async throws {
  let deadline = ContinuousClock.now + .seconds(5)
  var held = await condition()
  while !held, ContinuousClock.now < deadline {
    try await Task.sleep(for: .milliseconds(1))
    held = await condition()
  }
  try #require(held, "the condition never held", sourceLocation: sourceLocation)
}
