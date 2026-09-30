import Foundation

/// A launch log the UI smoke tests can read when a window never appears.
/// Under `-steno-ui-testing` only: milestones are appended to one file per
/// process in the temporary directory, and an uncaught Objective-C
/// exception (AppKit swallows those raised during scene setup and leaves
/// an app with menus and no window) is written with its stack. The smoke
/// test attaches the file beside XCTest's own hierarchy dump. Off the
/// UI-testing flag every call is a no-op.
enum UITestDiagnostics {
  /// Where `LaunchSmokeTests` looks: one file, truncated at each launch.
  /// `/tmp` rather than the process's temporary directory, which macOS
  /// gives every process separately, so the test runner can read it.
  static let logURL = URL(fileURLWithPath: "/tmp/steno-ui-test.log")

  private static let queue = DispatchQueue(label: "uno.schmid.steno.ui-test-diagnostics")
  private nonisolated(unsafe) static var enabled = false

  /// Starts the log and installs the exception handler. Called once from
  /// the app delegate before any window exists.
  static func start(enabled isEnabled: Bool) {
    guard isEnabled else { return }
    enabled = true
    try? "".write(to: logURL, atomically: true, encoding: .utf8)
    note("start pid \(ProcessInfo.processInfo.processIdentifier)")
    NSSetUncaughtExceptionHandler { exception in
      UITestDiagnostics.note(
        "uncaught exception \(exception.name.rawValue): \(exception.reason ?? "")\n"
          + exception.callStackSymbols.joined(separator: "\n"))
    }
  }

  /// Appends one line with a timestamp; safe from any thread.
  static func note(_ message: String) {
    guard enabled else { return }
    let line = "\(Date().timeIntervalSince1970) \(message)\n"
    queue.sync {
      guard let handle = try? FileHandle(forWritingTo: logURL) else { return }
      defer { try? handle.close() }
      _ = try? handle.seekToEnd()
      try? handle.write(contentsOf: Data(line.utf8))
    }
  }
}
