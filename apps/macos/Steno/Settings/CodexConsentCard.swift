import SwiftUI

/// The words the user reads before Steno may use the Codex sign-in, in one
/// place so onboarding and Settings say the same thing.
enum CodexConsentCopy {
  static let title = "Use your ChatGPT plan for summaries"

  static let body =
    "Steno will use the sign-in that the Codex command-line tool saved on this Mac (~/.codex/auth.json) and send your meeting transcripts to OpenAI under your ChatGPT plan. Audio never leaves your Mac."

  static let points = [
    "Summaries count against your ChatGPT plan's Codex limits, shared with your coding sessions.",
    "Steno refreshes the saved sign-in when it expires and writes the new one back to the same file, the same way Codex does.",
    "OpenAI allows tools like this today but has not promised to keep doing so. If it stops working, switch to an API key or a local model in Settings.",
  ]

  static let confirm = "Use my ChatGPT account"
  static let checkAgain = "Check again"

  /// Under the model picker once confirmed.
  static let usageFootnote =
    "Transcript text goes to OpenAI under your ChatGPT plan and counts against its Codex limits. Audio never leaves your Mac."
}

/// The consent card: title, explanation, the three points, the account line
/// from the sign-in on this Mac (or why there is none), and the one button
/// that lets Steno use it. Nothing is stored until that button.
struct CodexConsentCard: View {
  let status: LLMSettingsViewModel.CodexStatus
  let confirm: () async -> Void
  let checkAgain: () async -> Void
  /// Onboarding adds its own Skip beside the primary button.
  var trailing: AnyView? = nil
  @State private var working = false

  private var signedIn: Bool {
    if case .signedIn = status { return true }
    return false
  }

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.sm) {
      Text(CodexConsentCopy.title)
        .font(.steno(Theme.TextSize.sm, weight: .semibold))
        .foregroundStyle(Color.stenoStrong)
      Text(CodexConsentCopy.body)
        .font(.steno(Theme.TextSize.xs))
        .stenoLeading(Theme.TextSize.xs)
        .foregroundStyle(Color.stenoMutedForeground)
        .fixedSize(horizontal: false, vertical: true)
      VStack(alignment: .leading, spacing: Theme.Space.xs) {
        ForEach(CodexConsentCopy.points, id: \.self) { point in
          HStack(alignment: .top, spacing: Theme.Space.xs) {
            Text("•")
            Text(point).fixedSize(horizontal: false, vertical: true)
          }
          .font(.steno(Theme.TextSize.xs))
          .stenoLeading(Theme.TextSize.xs)
          .foregroundStyle(Color.stenoMutedForeground)
        }
      }
      switch status {
      case .signedIn(let account):
        MessageRow(kind: .info, text: "Signed in as \(account).")
      case .unavailable(let text):
        MessageRow(kind: .warning, text: text)
      case .unknown:
        EmptyView()
      }
      HStack(spacing: Theme.Space.sm) {
        Button(CodexConsentCopy.confirm) {
          Task {
            working = true
            defer { working = false }
            await confirm()
          }
        }
        .buttonStyle(StenoPrimaryButtonStyle())
        .disabled(!signedIn || working)
        .accessibilityIdentifier("codex-confirm")
        if !signedIn {
          Button(CodexConsentCopy.checkAgain) { Task { await checkAgain() } }
            .buttonStyle(StenoSecondaryButtonStyle())
            .accessibilityIdentifier("codex-check-again")
        }
        if let trailing { trailing }
        if working { ProgressView().controlSize(.small) }
      }
    }
    .accessibilityIdentifier("codex-consent-card")
  }
}
