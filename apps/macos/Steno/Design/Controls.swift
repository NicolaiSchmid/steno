import SwiftUI

/// The composed controls of the redesign that are not buttons or chips:
/// the nav row, the icon button, the search field, the text field box and
/// the segmented tabs. Boxes, fills and type follow the plan's components
/// table; hover and selection swap over `Motion.functional` and hold still
/// under Reduce Motion. No control here owns behaviour; callers pass the
/// selection, the query and the action.

/// A row in the nav column: glyph, label, optional count. Selected rows
/// sit on the `secondary` veil, hovered rows on `card`. Id `nav-<name>`.
struct NavRow: View {
  let name: String
  let symbol: String
  let label: String
  let count: Int?
  let isSelected: Bool
  let action: () -> Void
  @State private var hovering = false
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  init(
    name: String, symbol: String, label: String, count: Int? = nil, isSelected: Bool,
    action: @escaping () -> Void
  ) {
    self.name = name
    self.symbol = symbol
    self.label = label
    self.count = count
    self.isSelected = isSelected
    self.action = action
  }

  private var tint: Color { isSelected ? Color.stenoStrong : Color.stenoMutedForeground }

  private var fill: Color {
    if isSelected { return Color.stenoSecondary }
    return hovering ? Color.stenoCard : Color.clear
  }

  var body: some View {
    Button(action: action) {
      HStack(spacing: Theme.Control.rowInset) {
        Image(systemName: symbol)
          .font(.system(size: Theme.TextSize.base.size))
          .foregroundStyle(tint)
          .frame(width: Theme.Control.navGlyphWidth)
        Text(label)
          .font(.steno(Theme.TextSize.sm, weight: .medium))
          .foregroundStyle(tint)
          .lineLimit(1)
        Spacer(minLength: Theme.Space.sm)
        if let count {
          Text("\(count)")
            .font(.steno(Theme.TextSize.xxs))
            .monospacedDigit()
            .foregroundStyle(Color.stenoFaint)
        }
      }
      .padding(.horizontal, Theme.Control.rowInset)
      .frame(height: Theme.Control.navRowHeight)
      .frame(maxWidth: .infinity)
      .background(Theme.Radius.md.shape.fill(fill))
      .contentShape(Theme.Radius.md.shape)
    }
    .buttonStyle(.plain)
    .onHover { hovering = $0 }
    .animation(Motion.swap(reduceMotion: reduceMotion), value: hovering)
    .animation(Motion.swap(reduceMotion: reduceMotion), value: isSelected)
    .accessibilityIdentifier("nav-\(name)")
    .accessibilityAddTraits(isSelected ? [.isSelected] : [])
  }
}

/// A 28 pt round hairline button holding one 14 pt glyph; `muted` at rest,
/// `strong` on the `card` veil while hovered. The label is required because
/// the glyph is the only visible content.
struct IconButton: View {
  let systemName: String
  let label: String
  let action: () -> Void
  @State private var hovering = false
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  init(_ systemName: String, label: String, action: @escaping () -> Void) {
    self.systemName = systemName
    self.label = label
    self.action = action
  }

  var body: some View {
    Button(action: action) {
      Image(systemName: systemName)
        .font(.system(size: Theme.TextSize.sm.size, weight: .medium))
        .foregroundStyle(hovering ? Color.stenoStrong : Color.stenoMutedForeground)
        .frame(width: Theme.Control.iconButtonSize, height: Theme.Control.iconButtonSize)
        .background(Circle().fill(hovering ? Color.stenoCard : Color.clear))
        .overlay(Circle().strokeBorder(Color.stenoBorder, lineWidth: Theme.Space.hairline))
        .contentShape(Circle())
    }
    .buttonStyle(.plain)
    .onHover { hovering = $0 }
    .animation(Motion.swap(reduceMotion: reduceMotion), value: hovering)
    .help(label)
    .accessibilityLabel(label)
  }
}

/// The list column's search field: a 28 pt `raised` hairline box with the
/// glyph, `ring` while focused. The text field carries the accessibility id
/// so `app.textFields[id]` finds it.
struct SearchField: View {
  @Binding var text: String
  let placeholder: String
  let id: String
  @FocusState private var focused: Bool

  init(text: Binding<String>, placeholder: String = "Search", id: String = "search-meetings") {
    _text = text
    self.placeholder = placeholder
    self.id = id
  }

  var body: some View {
    HStack(spacing: Theme.Space.sm) {
      Image(systemName: "magnifyingglass")
        .font(.system(size: Theme.TextSize.sm.size))
        .foregroundStyle(Color.stenoFaint)
      TextField("", text: $text, prompt: Text(placeholder).foregroundStyle(Color.stenoFaint))
        .textFieldStyle(.plain)
        .font(.steno(Theme.TextSize.xs))
        .foregroundStyle(Color.stenoStrong)
        .focused($focused)
        .focusEffectDisabled()
        .accessibilityIdentifier(id)
    }
    .modifier(InputBox(focused: focused))
    .contentShape(Theme.Radius.md.shape)
    .onTapGesture { focused = true }
  }
}

/// The input box the search field and the text field share: 28 pt tall,
/// radius 8, `raised`, hairline `border`, `ring` while focused.
private struct InputBox: ViewModifier {
  let focused: Bool
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  func body(content: Content) -> some View {
    content
      .padding(.horizontal, Theme.Control.rowInset)
      .frame(height: Theme.Control.inputHeight)
      .background(Theme.Radius.md.shape.fill(Color.stenoRaised))
      .overlay(
        Theme.Radius.md.shape.strokeBorder(
          focused ? Color.stenoRing : Color.stenoBorder, lineWidth: Theme.Space.hairline)
      )
      .animation(Motion.swap(reduceMotion: reduceMotion), value: focused)
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
  /// while focused, 13 pt `strong` text. Apply to a `TextField`.
  func stenoTextField() -> some View {
    modifier(StenoTextFieldBox())
  }
}

/// A segmented control: a 28 pt `secondary` container at radius 8 holding
/// 24 pt cells at radius 6; the active cell is `raised` with `shadow-sm`.
/// Ids `tab-<id>`, the `isSelected` trait on the active cell.
struct SegmentedTabs<Tab: Hashable>: View {
  let tabs: [Tab]
  @Binding var selection: Tab
  let title: (Tab) -> String
  let id: (Tab) -> String
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  init(
    _ tabs: [Tab], selection: Binding<Tab>, title: @escaping (Tab) -> String,
    id: @escaping (Tab) -> String
  ) {
    self.tabs = tabs
    _selection = selection
    self.title = title
    self.id = id
  }

  var body: some View {
    HStack(spacing: 0) {
      ForEach(tabs, id: \.self) { tab in
        cell(tab)
      }
    }
    .padding(Theme.Space.xxs)
    .frame(height: Theme.Control.segmentContainerHeight)
    .background(Theme.Radius.md.shape.fill(Color.stenoSecondary))
    .fixedSize()
  }

  private func cell(_ tab: Tab) -> some View {
    let active = tab == selection
    return Button {
      withAnimation(Motion.swap(reduceMotion: reduceMotion)) { selection = tab }
    } label: {
      Text(title(tab))
        .font(.steno(Theme.TextSize.xs, weight: active ? .medium : .regular))
        .foregroundStyle(active ? Color.stenoStrong : Color.stenoMutedForeground)
        .padding(.horizontal, Theme.Control.rowInset)
        .frame(height: Theme.Control.segmentHeight)
        .background {
          if active {
            Theme.Radius.sm.shape.fill(Color.stenoRaised).stenoShadowSmall()
          }
        }
        .contentShape(Theme.Radius.sm.shape)
    }
    .buttonStyle(.plain)
    .accessibilityIdentifier("tab-\(id(tab))")
    .accessibilityAddTraits(active ? [.isSelected] : [])
  }
}

extension SegmentedTabs where Tab: RawRepresentable, Tab.RawValue == String {
  /// Tabs whose raw value is the id, as `MeetingDetailViewModel.Tab`.
  init(_ tabs: [Tab], selection: Binding<Tab>, title: @escaping (Tab) -> String) {
    self.init(tabs, selection: selection, title: title, id: \.rawValue)
  }
}

#if DEBUG
  private struct ControlsPreview: View {
    enum Tab: String, CaseIterable {
      case summary, transcript, tasks, scratchpad
    }

    @State private var tab = Tab.summary
    @State private var query = ""
    @State private var name = ""
    @State private var selected = "all"

    var body: some View {
      VStack(alignment: .leading, spacing: Theme.Space.lg) {
        VStack(spacing: Theme.Space.xxs) {
          NavRow(
            name: "all", symbol: "rectangle.stack", label: "All", count: 12,
            isSelected: selected == "all"
          ) { selected = "all" }
          NavRow(
            name: "failed", symbol: "exclamationmark.triangle", label: "Failed", count: 1,
            isSelected: selected == "failed"
          ) { selected = "failed" }
          NavRow(name: "settings", symbol: "gearshape", label: "Settings", isSelected: false) {}
        }
        .frame(width: 220)
        SegmentedTabs(Tab.allCases, selection: $tab) { $0.rawValue.capitalized }
        HStack(spacing: Theme.Space.sm) {
          SearchField(text: $query)
            .frame(width: 240)
          IconButton("ellipsis", label: "Actions") {}
        }
        TextField("Speaker name", text: $name)
          .stenoTextField()
          .frame(width: 240)
      }
    }
  }

  #Preview("Controls") {
    PreviewPair { ControlsPreview() }
      .frame(width: 720, height: 360)
  }
#endif
