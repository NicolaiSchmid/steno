import Foundation

/// `transform` over `items` with at most `limit` in flight, results in
/// input order. The first error cancels the rest and is rethrown.
func mapBounded<Item: Sendable, Result: Sendable>(
  _ items: [Item], limit: Int,
  _ transform: @escaping @Sendable (Item) async throws -> Result
) async throws -> [Result] {
  var results: [(index: Int, result: Result)] = []
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
    while let result = try await group.next() {
      results.append(result)
      addNext()
    }
  }
  return results.sorted { $0.index < $1.index }.map(\.result)
}
