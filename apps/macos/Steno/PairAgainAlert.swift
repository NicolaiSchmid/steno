import AppKit

/// The launch's question when the keychain lost or replaced the identity
/// the paired phones know (`HandoverIdentityGuard.loadOrPairAgain`): Pair
/// Again, after a second warning, or Not Now, which leaves everything as it
/// is until the next launch asks again.
@MainActor
enum PairAgainAlert {
  /// One of the two alerts. Its first button, Pair Again, confirms.
  struct Prompt: Equatable {
    let message: String
    let information: String
    let cancel: String
    /// The warning: Pair Again is destructive and Cancel the default.
    let destructive: Bool

    /// Pair Again's key: Return on the question, none on the warning.
    var confirmKey: String { destructive ? "" : "\r" }
    /// The cancel button's key: Escape on the question, Return on the
    /// warning (and Escape, through `ask()`'s monitor).
    var cancelKey: String { destructive ? "\r" : "\u{1b}" }
  }

  static let question = Prompt(
    message: "Steno can't find this Mac's phone pairing",
    information: "Your phones can't send recordings to this Mac right now. This can happen after "
      + "the Mac's password was reset and a new keychain was created.\n\n"
      + "If you can bring back your old keychain, choose Not Now and open Steno again "
      + "afterwards. Otherwise, pair your phones again.",
    cancel: "Not Now", destructive: false)

  /// The words of the Tauri app's warning (`pair_again_prompt` in
  /// `crates/steno-host/src/host.rs`), with where to pair and what stays.
  static let warning = Prompt(
    message: "Pair every phone again?",
    information: "Steno makes a new pairing for this Mac and removes the phones paired with it. "
      + "Each phone must scan a new code in Settings > iPhone before it can send recordings "
      + "again. Recordings already on this Mac stay.",
    cancel: "Cancel", destructive: true)

  static func ask() -> Bool {
    NSApp.activate()
    return ask { prompt in
      let alert = alert(for: prompt)
      // A button holds one key, so on the warning, where Cancel holds
      // Return, Escape reaches it through a monitor.
      let escape = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { event in
        guard event.charactersIgnoringModifiers == "\u{1b}" else { return event }
        alert.buttons[1].performClick(nil)
        return nil
      }
      defer { escape.map(NSEvent.removeMonitor) }
      return alert.runModal() == .alertFirstButtonReturn
    }
  }

  /// Pair Again only when both the question and the warning confirm it.
  static func ask(confirm: (Prompt) -> Bool) -> Bool {
    confirm(question) && confirm(warning)
  }

  /// The alert for `prompt`: Pair Again first, the cancel button second.
  private static func alert(for prompt: Prompt) -> NSAlert {
    let alert = NSAlert()
    alert.messageText = prompt.message
    alert.informativeText = prompt.information
    let pairAgain = alert.addButton(withTitle: "Pair Again")
    pairAgain.hasDestructiveAction = prompt.destructive
    pairAgain.keyEquivalent = prompt.confirmKey
    alert.addButton(withTitle: prompt.cancel).keyEquivalent = prompt.cancelKey
    return alert
  }
}
