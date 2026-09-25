import Foundation

/// `transform` over `items` with at most `limit` in flight, results in
/// input order. The first error cancels the rest and is rethrown.
func mapBounded<Item: Sendable, Result: Sendable>(
  _ items: [Item], limit: Int,
  _ transform: @escaping @Sendable (Item) async throws -> Result
) async throws -> [Result] {
  guard !items.isEmpty else { return [] }
  var results = [Result?](repeating: nil, count: items.count)
  try await withThrowingTaskGroup(of: (Int, Result).self) { group in
    var next = 0
    func addNext() {
      guard next < items.count else { return }
      let index = next
      let item = items[index]
      next += 1
      group.addTask { (index, try await transform(item)) }
    }
    for _ in 0..<max(1, limit) { addNext() }
    while let (index, result) = try await group.next() {
      results[index] = result
      addNext()
    }
  }
  return results.map { $0! }
}
