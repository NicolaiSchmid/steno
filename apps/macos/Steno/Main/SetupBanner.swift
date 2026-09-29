import SwiftUI

/// Row 1 of the detail header stack: a launch-time reminder that summaries
/// or export are off, shown when at least one meeting exists, the
/// configuration is incomplete (`AppController.setupBannerMessage`, which
/// follows the stored settings) and "Not now" was not pressed this launch
/// (`AppController.setupBannerDismissed`). The per-meeting rows in the
/// detail pane carry the signal permanently, so the banner needs no second
/// flag. One `Card`: the `info` dot and the sentence on the first row, the
/// actions on the second, so the sentence never competes with the buttons
/// for width (at the 440 pt pane minimum it wrapped to one character per
/// line beside them) and the card is not a tinted box inside a white box.
/// Ids `setup-banner`, `setup-summaries`, `choose-vault` and
/// `banner-not-now`; the UI smoke test matches ids, not copy.
struct SetupBanner: View {
  let controller: AppController
  let hasMeetings: Bool
  @Environment(\.openSettings) private var openSettings
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  private var message: SetupBannerMessage? {
    guard hasMeetings, !controller.setupBannerDismissed else { return nil }
    return controller.setupBannerMessage
  }

  var body: some View {
    VStack(spacing: 0) {
      if let message {
        Card {
          VStack(alignment: .leading, spacing: Theme.Space.md) {
            // The line limit bounds the width-0 layout probe, which the
            // empty state paid for once (`EmptyState.bodyWidth`); the
            // longest sentence takes three lines at the pane minimum.
            StatusLine(color: Color.stenoInfo, text: message.text)
              .lineLimit(4)
              .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: Theme.Space.sm) {
              if message.offersSummaries {
                Button(SetupCopy.setUpSummaries) {
                  controller.openSettings(.summaries, with: openSettings)
                }
                .buttonStyle(StenoPrimaryButtonStyle())
                .accessibilityIdentifier("setup-summaries")
              }
              if message.offersVault {
                // Primary when it is the only fix on offer, secondary beside
                // "Set up summaries".
                if message.offersSummaries {
                  chooseVaultButton.buttonStyle(StenoSecondaryButtonStyle())
                } else {
                  chooseVaultButton.buttonStyle(StenoPrimaryButtonStyle())
                }
              }
              Button("Not now") { controller.dismissSetupBanner() }
                .buttonStyle(StenoGhostButtonStyle())
                .accessibilityIdentifier("banner-not-now")
            }
          }
          .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.horizontal, Theme.Space.xxl)
        .padding(.top, Theme.Space.lg)
        .transition(.opacity)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("setup-banner")
      }
    }
    .animation(Motion.swap(reduceMotion: reduceMotion), value: message)
  }

  private var chooseVaultButton: some View {
    Button(SetupCopy.chooseVault) {
      controller.openSettings(.export, with: openSettings)
    }
    .accessibilityIdentifier("choose-vault")
  }
}
