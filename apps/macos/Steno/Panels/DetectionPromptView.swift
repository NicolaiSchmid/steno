import SwiftUI

/// The detection prompt in the panel's pill language: "<App> opened the
/// microphone", the one-line subtitle, one primary Record button with the
/// glyph, an X, and the draining countdown hairline along the bottom. No
/// number: nothing is at stake when the prompt closes. Keyboard shortcuts
/// are absent because a non-activating panel is never key.
struct DetectionPromptView: View {
  let model: DetectionPromptViewModel

  var body: some View {
    PromptBody(
      appName: model.appName, fractionRemaining: model.countdown.fractionRemaining,
      record: { Task { await model.start() } },
      dismiss: { Task { await model.dismiss() } })
  }
}

/// The prompt's layout without the model behind it, so the previews render
/// the countdown at any fraction.
struct PromptBody: View {
  let appName: String
  let fractionRemaining: Double
  let record: () -> Void
  let dismiss: () -> Void

  var body: some View {
    HStack(spacing: Theme.Space.lg) {
      VStack(alignment: .leading, spacing: Theme.Space.xxs) {
        Text("\(appName) opened the microphone")
          .font(.steno(Theme.TextSize.sm, weight: .semibold))
          .foregroundStyle(Color.stenoStrong)
          .lineLimit(1)
          .truncationMode(.tail)
          .accessibilityIdentifier("prompt-title")
        Text("Record with Steno? Audio stays on this Mac.")
          .font(.steno(Theme.TextSize.xxs))
          .foregroundStyle(Color.stenoMutedForeground)
          .lineLimit(1)
      }
      Spacer(minLength: 0)
      Button(action: record) {
        HStack(spacing: Theme.Space.sm) {
          Image(systemName: BubbleGlyph.symbolName)
            .font(.system(size: PanelMetrics.glyphSize, weight: .semibold))
          Text("Record")
        }
      }
      .buttonStyle(StenoPrimaryButtonStyle())
      .accessibilityLabel("Record with Steno")
      .accessibilityIdentifier("prompt-record")
      PromptDismissButton(action: dismiss)
    }
    .padding(.leading, Theme.Space.lg)
    .padding(.trailing, Theme.Space.sm)
    .frame(height: PanelMetrics.promptHeight)
    .frame(minWidth: PanelMetrics.promptMinWidth, maxWidth: PanelMetrics.promptMaxWidth)
    .overlay(alignment: .bottom) {
      CountdownHairline(fractionRemaining: fractionRemaining)
        .padding(.horizontal, PanelMetrics.hairlineInset)
        .padding(.bottom, PanelMetrics.hairlineBottom)
    }
    .modifier(PanelBar())
    .accessibilityElement(children: .contain)
    .accessibilityHint("Closes on its own after a minute.")
    .accessibilityIdentifier("detection-prompt")
  }
}

/// The prompt's X: a 24 pt plain button with an 11 pt `xmark` in `faint`,
/// `strong` on hover, a 28 pt hit area. Label "Not now".
struct PromptDismissButton: View {
  let action: () -> Void
  @State private var hovering = false
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  var body: some View {
    Button(action: action) {
      Image(systemName: "xmark")
        .font(.system(size: PanelMetrics.dismissGlyphSize, weight: .semibold))
        .foregroundStyle(hovering ? Color.stenoStrong : Color.stenoFaint)
        .frame(width: PanelMetrics.dismissSize, height: PanelMetrics.dismissSize)
        .frame(width: PanelMetrics.dismissHitSize, height: PanelMetrics.dismissHitSize)
        .contentShape(Rectangle())
    }
    .buttonStyle(.plain)
    .onHover { hovering = $0 }
    .animation(Motion.swap(reduceMotion: reduceMotion), value: hovering)
    .help("Not now")
    .accessibilityLabel("Not now")
    .accessibilityIdentifier("prompt-dismiss")
  }
}

#if DEBUG
  #Preview("Detection prompt") {
    PreviewPair {
      VStack(alignment: .leading, spacing: Theme.Space.lg) {
        PromptBody(appName: "Zoom", fractionRemaining: 1, record: {}, dismiss: {})
        PromptBody(appName: "FaceTime", fractionRemaining: 0.5, record: {}, dismiss: {})
        PromptBody(
          appName: "Microsoft Teams (work or school)", fractionRemaining: 0.06, record: {},
          dismiss: {})
      }
    }
    .frame(width: 1080, height: 300)
  }
#endif
