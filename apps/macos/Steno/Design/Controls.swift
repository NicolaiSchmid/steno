import SwiftUI

/// The composed controls the SwiftUI surfaces still draw: the text field
/// box and its secure twin, used by Settings and onboarding. The nav row,
/// the icon button, the search field and the segmented tabs left with the
/// main window, which the web UI renders (plan
/// `2026-09-29-macos-webview-ui.md`, WP2). Boxes, fills and type follow the
/// plan's components table; focus swaps over `Motion.functional` and holds
/// still under Reduce Motion. No control here owns behaviour; callers pass
/// the text.

/// The input box: 28 pt tall, radius 8, `raised`, hairline `border`; while
/// focused the hairline turns `ring` and a 2 pt `ring` stroke sits outside
/// the box, so keyboard focus reads beyond the caret.
private struct InputBox: ViewModifier {
  let focused: Bool
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  func body(content: Content) -> some View {
    content
      .padding(.horizontal, Theme.Control.rowInset)
      .frame(height: Theme.Control.inputHeight)
      .background(Theme.Radius.md.shape.fill(Color.stenoRaised))
      .overlay(Theme.Radius.md.shape.hairline(focused ? Color.stenoRing : Color.stenoBorder))
      .overlay {
        if focused { focusRing }
      }
      .animation(Motion.swap(reduceMotion: reduceMotion), value: focused)
  }

  private var focusRing: some View {
    RoundedRectangle(cornerRadius: Theme.Radius.md.rawValue + Theme.Space.xxs, style: .continuous)
      .strokeBorder(Color.stenoRing, lineWidth: Theme.Space.xxs)
      .padding(-Theme.Space.xxs)
  }
}

/// The hairline text field of the plan's `StenoTextFieldStyle` row, shipped
/// as a modifier: `TextFieldStyle._body` is a nonisolated requirement and
/// Swift 6.1 cannot build a view holding `@FocusState` from it, whereas a
/// `View` extension is main-actor inferred. Replaces `.roundedBorder`.
private struct StenoTextFieldBox: ViewModifier {
  @FocusState private var focused: Bool

  func body(content: Content) -> some View {
    content
      .textFieldStyle(.plain)
      .font(.steno(Theme.TextSize.xs))
      .foregroundStyle(Color.stenoStrong)
      .focused($focused)
      .focusEffectDisabled()
      .modifier(InputBox(focused: focused))
  }
}

extension View {
  /// The hairline text field box: 28 pt tall, radius 8, `raised`, `ring`
  /// while focused, 13 pt `strong` text. Apply to a `TextField`. The
  /// modifier cannot reach the field's prompt, so a field with a placeholder
  /// is `StenoTextField(_:text:)`, which passes the `faint` prompt once.
  func stenoTextField() -> some View {
    modifier(StenoTextFieldBox())
  }
}

/// The hairline text field with its placeholder in `faint`: the title is
/// the accessibility label and the prompt is the visible placeholder.
struct StenoTextField: View {
  @Binding var text: String
  let placeholder: String

  init(_ placeholder: String, text: Binding<String>) {
    self.placeholder = placeholder
    _text = text
  }

  var body: some View {
    TextField(placeholder, text: $text, prompt: Text(placeholder).foregroundStyle(Color.stenoFaint))
      .stenoTextField()
  }
}

/// `StenoTextField` over a `SecureField`, for API keys: the same box, the
/// same `faint` prompt, the entry hidden.
struct StenoSecureField: View {
  @Binding var text: String
  let placeholder: String

  init(_ placeholder: String, text: Binding<String>) {
    self.placeholder = placeholder
    _text = text
  }

  var body: some View {
    SecureField(
      placeholder, text: $text, prompt: Text(placeholder).foregroundStyle(Color.stenoFaint)
    )
    .stenoTextField()
  }
}

#if DEBUG
  private struct ControlsPreview: View {
    @State private var name = ""
    @State private var secret = ""

    var body: some View {
      VStack(alignment: .leading, spacing: Theme.Space.lg) {
        HStack(spacing: Theme.Space.sm) {
          StenoTextField("Speaker name", text: $name)
            .frame(width: 240)
          // The focused box, rendered statically since a preview cannot hold focus.
          Text("Focused")
            .font(.steno(Theme.TextSize.xs))
            .foregroundStyle(Color.stenoStrong)
            .frame(maxWidth: .infinity, alignment: .leading)
            .modifier(InputBox(focused: true))
            .frame(width: 240)
        }
        StenoSecureField("API key", text: $secret)
          .frame(width: 240)
      }
    }
  }

  #Preview("Controls") {
    PreviewPair { ControlsPreview() }
      .frame(width: 720, height: 240)
  }
#endif
