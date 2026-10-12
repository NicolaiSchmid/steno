import AppKit

/// The launch's question when the keychain lost or replaced the identity
/// the paired phones know (`HandoverIdentityGuard.loadOrPairAgain`): Pair
/// Again, after a second warning, or Not Now, which leaves everything as it
/// is until the next launch asks again.
@MainActor
enum PairAgainAlert {
  static func ask() -> Bool {
    NSApp.activate()
    return confirm(
      "Steno can't find this Mac's phone pairing",
      "Your phones can't send recordings to this Mac right now. This can happen after "
        + "the Mac's password was reset and a new keychain was created.\n\n"
        + "If you can bring back your old keychain, choose Not Now and open Steno again "
        + "afterwards. Otherwise, pair your phones again.",
      cancel: "Not Now")
      && confirm(
        "Pair every phone again?",
        "Steno forgets the phones paired with this Mac. Each phone can send recordings "
          + "again once you pair it in Settings > iPhone. Recordings already on this Mac stay.",
        cancel: "Cancel", destructive: true)
  }

  /// One alert whose first button, Pair Again, confirms; Escape cancels.
  private static func confirm(
    _ message: String, _ information: String, cancel: String, destructive: Bool = false
  ) -> Bool {
    let alert = NSAlert()
    alert.messageText = message
    alert.informativeText = information
    alert.addButton(withTitle: "Pair Again").hasDestructiveAction = destructive
    alert.addButton(withTitle: cancel).keyEquivalent = "\u{1b}"
    return alert.runModal() == .alertFirstButtonReturn
  }
}
