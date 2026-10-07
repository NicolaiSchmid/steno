import Testing

/// Polls `condition` every millisecond for at most 50 seconds; else fails
/// at the caller's line and throws, so the test stops there. A starved
/// runner can take seconds to reach a condition that no seam signals; the
/// wait ends before the caller's one-minute time limit, so a condition that
/// never holds is reported at its line rather than as a timeout.
func until(
  _ condition: () async -> Bool, sourceLocation: SourceLocation = #_sourceLocation
) async throws {
  let deadline = ContinuousClock.now + .seconds(50)
  var held = await condition()
  while !held, ContinuousClock.now < deadline {
    try await Task.sleep(for: .milliseconds(1))
    held = await condition()
  }
  try #require(held, "the condition never held", sourceLocation: sourceLocation)
}
