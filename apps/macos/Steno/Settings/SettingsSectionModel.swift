import Foundation

/// What every Settings section's view model provides: a `load()` the host
/// runs when the window opens and the error pair the page renders, a plain
/// sentence in `error` with the original text in `errorDetails`. Main-actor
/// classes are Sendable, so the existential can be captured by a task.
@MainActor
protocol SettingsSectionModel: AnyObject, Sendable {
  var error: String? { get set }
  var errorDetails: String? { get set }
  func load() async
}

extension SettingsSectionModel {
  func fail(_ message: String, _ error: any Error) {
    self.error = message
    errorDetails = String(describing: error)
  }

  func clearError() {
    error = nil
    errorDetails = nil
  }
}
