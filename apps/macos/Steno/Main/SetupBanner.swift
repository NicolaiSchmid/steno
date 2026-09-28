import SwiftUI

/// Row 1 of the detail header stack: a launch-time reminder that summaries
/// or export are off, shown when at least one meeting exists, the
/// configuration is incomplete (`AppController.setupBannerMessage`, which
/// follows the stored settings) and "Not now" was not pressed this launch
/// (`AppController.setupBannerDismissed`). The per-meeting rows in the
/// detail pane carry the signal permanently, so the banner needs no second
/// flag. Ids `setup-banner`, `setup-summaries`, `choose-vault` and
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
        Card(padding: Theme.Space.md) {
          HStack(alignment: .center, spacing: Theme.Space.md) {
            MessageRow(kind: .info, text: message.text)
            HStack(spacing: Theme.Space.sm) {
              if message.offersSummaries {
                Button(SetupCopy.setUpSummaries) {
                  controller.openSettings(.llm, with: openSettings)
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
            .fixedSize()
          }
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
      controller.openSettings(.obsidian, with: openSettings)
    }
    .accessibilityIdentifier("choose-vault")
  }
}
