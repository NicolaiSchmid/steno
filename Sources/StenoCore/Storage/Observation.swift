import Foundation
import GRDB

extension DatabaseWriter {
  /// Bridges a GRDB `ValueObservation` to an `AsyncThrowingStream` on the task
  /// scheduler: the current value first, then a value after every change to
  /// the tracked region. Cancelling the consumer stops the observation.
  func stream<Value: Sendable>(
    _ observation: ValueObservation<ValueReducers.Fetch<Value>>
  ) -> AsyncThrowingStream<Value, any Error> {
    AsyncThrowingStream { continuation in
      let task = Task {
        do {
          for try await value in observation.values(in: self, scheduling: .task) {
            continuation.yield(value)
          }
          continuation.finish()
        } catch {
          continuation.finish(throwing: error)
        }
      }
      continuation.onTermination = { _ in task.cancel() }
    }
  }
}
