import AppKit

/// The launch's question when the keychain lost or replaced the identity
/// the paired phones know (`HandoverIdentityGuard.loadOrPairAgain`): Pair
/// Again, after a second warning, or Not Now, which leaves everything as it
/// is until the next launch asks again.
@MainActor
enum PairAgainAlert {
  static func ask() async -> Bool {
    NSApp.activate()
    let question = NSAlert()
    question.messageText = "Steno can't find this Mac's phone pairing"
    question.informativeText =
      "Your phones can't send recordings to this Mac right now. This can happen after "
      + "the Mac's password was reset and a new keychain was created.\n\n"
      + "If you can bring back your old keychain, choose Not Now and open Steno again "
      + "afterwards. Otherwise, pair your phones again."
    question.addButton(withTitle: "Pair Again")
    question.addButton(withTitle: "Not Now").keyEquivalent = "\u{1b}"
    guard question.runModal() == .alertFirstButtonReturn else { return false }

    let warning = NSAlert()
    warning.alertStyle = .warning
    warning.messageText = "Pair every phone again?"
    warning.informativeText =
      "Steno forgets the phones paired with this Mac. Each phone can send recordings "
      + "again once you pair it in Settings > iPhone. Recordings already on this Mac stay."
    warning.addButton(withTitle: "Pair Again").hasDestructiveAction = true
    warning.addButton(withTitle: "Cancel").keyEquivalent = "\u{1b}"
    return warning.runModal() == .alertFirstButtonReturn
  }
}
