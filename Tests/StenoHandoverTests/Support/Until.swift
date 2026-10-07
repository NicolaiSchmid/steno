import Testing

/// Polls `condition` every millisecond for at most a minute; else fails at
/// the caller's line and throws, so the test stops there. The caller's time
/// limit bounds the wait: a starved runner can take seconds to reach a
/// condition that no seam signals.
func until(
  _ condition: () async -> Bool, sourceLocation: SourceLocation = #_sourceLocation
) async throws {
  let deadline = ContinuousClock.now + .seconds(60)
  var held = await condition()
  while !held, ContinuousClock.now < deadline {
    try await Task.sleep(for: .milliseconds(1))
    held = await condition()
  }
  try #require(held, "the condition never held", sourceLocation: sourceLocation)
}
