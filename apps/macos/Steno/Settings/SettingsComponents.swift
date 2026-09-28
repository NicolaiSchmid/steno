import AppKit
import SwiftUI

/// The pieces the Settings sections share: a section header, a footnote,
/// the leading label of a row, a status with its details folded away, a
/// permission row, a folder row, the sidebar row and the one sheet shape.

/// What every section's view model provides: a `load()` for the page's
/// `.task` and the error pair `SettingsErrorRow` renders, a plain sentence
/// in `error` with the original text in `errorDetails`. Main-actor classes
/// are Sendable, so the existential can be captured by the page's `.task`.
@MainActor
protocol SettingsSectionModel: AnyObject, Sendable {
  var error: String? { get set }
  var errorDetails: String? { get set }
  func load() async
}

extension SettingsSectionModel {
  func fail(_ message: String, _ error: any Error) {
    self.error = message
    errorDetails = String(describing: error)
  }

  func clearError() {
    error = nil
    errorDetails = nil
  }
}

/// Sizes the Settings window alone needs; everything else comes from
/// `Theme`. The icon well matches the design system's icon button.
enum SettingsMetrics {
  static let iconWell: CGFloat = Theme.Control.iconButtonSize
  static let qrCodeSize: CGFloat = 220
}

struct SettingsHeader: View {
  let section: SettingsSection

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.xs) {
      Text(section.title)
        .font(.steno(Theme.TextSize.lg, weight: .semibold))
        .foregroundStyle(Color.stenoStrong)
      Text(section.purpose)
        .font(.steno(Theme.TextSize.xs))
        .foregroundStyle(Color.stenoMutedForeground)
        .fixedSize(horizontal: false, vertical: true)
    }
    .frame(maxWidth: .infinity, alignment: .leading)
    .padding(.horizontal, Theme.Space.xl)
    .padding(.top, Theme.Space.xl)
    .padding(.bottom, Theme.Space.sm)
  }
}

/// Explanatory text under a control: the notes tier, 12 pt `faint`.
struct Footnote: View {
  let text: String

  init(_ text: String) {
    self.text = text
  }

  var body: some View {
    Text(text)
      .font(.steno(Theme.TextSize.xxs))
      .foregroundStyle(Color.stenoFaint)
      .fixedSize(horizontal: false, vertical: true)
  }
}

/// The leading text of a custom row, sized like the labels of the native
/// controls around it (13 pt, the form's label colour), with a 12 pt
/// `faint` subtitle when the row has two lines.
struct SettingsRowLabel: View {
  let title: String
  var subtitle: String? = nil

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.xxs) {
      Text(title)
        .font(.steno(Theme.TextSize.xs))
      if let subtitle {
        Text(subtitle)
          .font(.steno(Theme.TextSize.xxs))
          .foregroundStyle(Color.stenoFaint)
          .lineLimit(1)
          .truncationMode(.middle)
      }
    }
  }
}

/// A plain sentence, with the original error text behind "Details".
struct SettingsErrorRow: View {
  let message: String
  let details: String?

  var body: some View {
    SettingsStatusRow(kind: .error, message: message, details: details)
  }
}

/// A status sentence of any kind, with technical detail folded away.
struct SettingsStatusRow: View {
  let kind: MessageRow.Kind
  let message: String
  var details: String? = nil
  @State private var expanded = false

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.xs) {
      MessageRow(kind: kind, text: message)
      if let details, !details.isEmpty {
        DisclosureGroup("Details", isExpanded: $expanded) {
          Text(details)
            .font(.steno(Theme.TextSize.xxs).monospaced())
            .foregroundStyle(Color.stenoMutedForeground)
            .textSelection(.enabled)
            .fixedSize(horizontal: false, vertical: true)
            .padding(.top, Theme.Space.xs)
        }
        .font(.steno(Theme.TextSize.xxs))
        .foregroundStyle(Color.stenoFaint)
      }
    }
  }
}

/// One permission: its state glyph, title, the explanation while it is not
/// granted, and the action that fits the state.
struct PermissionRow: View {
  let kind: PermissionKind
  let state: PermissionState
  let isRequesting: Bool
  let request: @MainActor () -> Void
  let openSystemSettings: @MainActor () -> Void

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.xs) {
      HStack(spacing: Theme.Space.sm) {
        stateIcon
        SettingsRowLabel(title: kind.title)
        Spacer()
        if isRequesting {
          ProgressView().controlSize(.small)
        }
        switch state {
        case .granted:
          Text("Allowed")
            .font(.steno(Theme.TextSize.xs))
            .foregroundStyle(Color.stenoFaint)
        case .denied:
          Button("Open System Settings") { openSystemSettings() }
        case .unknown:
          Button(kind == .systemAudio ? "Run the test recording" : "Allow") { request() }
            .disabled(isRequesting)
        }
      }
      if state != .granted {
        Footnote(kind.explanation)
      }
      if isRequesting, kind == .systemAudio {
        Footnote("Listening for the test tone, up to 30 seconds…")
      }
    }
  }

  @ViewBuilder
  private var stateIcon: some View {
    switch state {
    case .granted:
      Image(systemName: "checkmark.circle.fill").foregroundStyle(Color.stenoLiveBright)
    case .denied:
      Image(systemName: "xmark.circle.fill").foregroundStyle(Color.stenoDestructive)
    case .unknown:
      Image(systemName: "circle").foregroundStyle(Color.stenoGhost)
    }
  }
}

/// A folder as a folder: glyph, name, the full path as a tooltip, and the
/// actions.
struct FolderRow: View {
  let label: String
  let url: URL?
  let choose: @MainActor (URL) -> Void
  var reveal: (@MainActor () -> Void)? = nil

  var body: some View {
    HStack(spacing: Theme.Space.sm) {
      SettingsRowLabel(title: label)
      Spacer()
      if let url {
        HStack(spacing: Theme.Space.xs) {
          Image(systemName: "folder")
            .foregroundStyle(Color.stenoMutedForeground)
          Text(url.lastPathComponent)
            .font(.steno(Theme.TextSize.xs))
            .foregroundStyle(Color.stenoMutedForeground)
            .lineLimit(1)
            .truncationMode(.middle)
        }
        .help(url.path)
        if let reveal {
          Button("Show in Finder") { reveal() }
        }
      }
      Button("Choose…") { present() }
    }
  }

  private func present() {
    let panel = NSOpenPanel()
    panel.canChooseDirectories = true
    panel.canChooseFiles = false
    panel.canCreateDirectories = true
    panel.allowsMultipleSelection = false
    panel.directoryURL = url
    panel.prompt = "Use folder"
    if panel.runModal() == .OK, let chosen = panel.url {
      choose(chosen)
    }
  }
}

/// The sidebar entry: an icon well, the title and the status subtitle. The
/// selected row hands its text to the list's hierarchical styles so it
/// inverts with the selection highlight; resting rows use the ladder.
struct SettingsSidebarRow: View {
  let section: SettingsSection
  let subtitle: String?
  var isSelected = false

  var body: some View {
    HStack(spacing: Theme.Space.md) {
      Image(systemName: section.systemImage)
        .font(.steno(Theme.TextSize.xs, weight: .medium))
        .foregroundStyle(titleStyle)
        .frame(width: SettingsMetrics.iconWell, height: SettingsMetrics.iconWell)
        .background(
          Theme.Radius.sm.shape
            .fill(Color.stenoSecondary)
        )
        .overlay(
          Theme.Radius.sm.shape.hairline())
      VStack(alignment: .leading, spacing: Theme.Space.xxs) {
        Text(section.title)
          .font(.steno(Theme.TextSize.xs, weight: .medium))
          .foregroundStyle(titleStyle)
        if let subtitle {
          Text(subtitle)
            .font(.steno(Theme.TextSize.xxxs))
            .foregroundStyle(subtitleStyle)
            .lineLimit(1)
        }
      }
    }
    .accessibilityElement(children: .combine)
    .accessibilityIdentifier("settings-\(section.rawValue)")
  }

  private var titleStyle: AnyShapeStyle {
    isSelected ? AnyShapeStyle(.primary) : AnyShapeStyle(Color.stenoForeground)
  }

  private var subtitleStyle: AnyShapeStyle {
    isSelected ? AnyShapeStyle(.secondary) : AnyShapeStyle(Color.stenoFaint)
  }
}

/// The one sheet shape Settings presents: a title, the body, and the closing
/// button trailing. Done is the default action in the primary style; Cancel
/// is a secondary button bound to Escape.
struct SettingsSheet<Content: View>: View {
  enum Dismissal {
    case done
    case cancel
  }

  let title: String
  let dismissal: Dismissal
  let width: CGFloat
  var height: CGFloat? = nil
  let dismiss: @MainActor () -> Void
  @ViewBuilder let content: () -> Content

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.lg) {
      Text(title)
        .font(.steno(Theme.TextSize.lg, weight: .semibold))
        .foregroundStyle(Color.stenoStrong)
      content()
      HStack {
        Spacer()
        switch dismissal {
        case .done:
          Button("Done") { dismiss() }
            .buttonStyle(StenoPrimaryButtonStyle())
            .keyboardShortcut(.defaultAction)
        case .cancel:
          Button("Cancel") { dismiss() }
            .buttonStyle(StenoSecondaryButtonStyle())
            .keyboardShortcut(.cancelAction)
        }
      }
    }
    .padding(Theme.Space.xl)
    .frame(width: width, height: height)
    .background(Color.stenoBackground)
  }
}

extension View {
  /// Text fields save through `commit` on Return, when focus leaves a field
  /// and when the page goes away. `focus` is the page's `@FocusState` value.
  func commitsFields<Field: Hashable>(
    focus: Field?, _ commit: @escaping @MainActor () -> Void
  ) -> some View {
    onSubmit { commit() }
      .onChange(of: focus) { old, _ in
        if old != nil { commit() }
      }
      .onDisappear { commit() }
  }
}
