import AppKit
import SwiftUI

/// The rows the Settings sections share: a section header, a footnote, an
/// error with its details folded away, a permission row, a folder row and
/// the sidebar row.

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

/// Explanatory text under a control: 13 pt, the faint tier.
struct Footnote: View {
  let text: String

  init(_ text: String) {
    self.text = text
  }

  var body: some View {
    Text(text)
      .font(.steno(Theme.TextSize.xs))
      .foregroundStyle(Color.stenoFaint)
      .fixedSize(horizontal: false, vertical: true)
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
  let details: String?
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
        Text(kind.title)
          .font(.steno(Theme.TextSize.sm))
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
  let choosePrompt: String
  let choose: @MainActor (URL) -> Void
  var reveal: (@MainActor () -> Void)? = nil

  var body: some View {
    HStack(spacing: Theme.Space.sm) {
      Text(label)
        .font(.steno(Theme.TextSize.sm))
      Spacer()
      if let url {
        HStack(spacing: Theme.Space.xs) {
          Image(systemName: "folder")
            .foregroundStyle(Color.stenoFaint)
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
      Button(url == nil ? choosePrompt : "Choose…") { present() }
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

/// The sidebar entry: an icon well, the title and the status subtitle.
struct SettingsSidebarRow: View {
  let section: SettingsSection
  let subtitle: String?

  var body: some View {
    HStack(spacing: Theme.Space.md) {
      Image(systemName: section.systemImage)
        .font(.system(size: 13, weight: .medium))
        .foregroundStyle(Color.stenoForeground)
        .frame(width: 28, height: 28)
        .background(
          RoundedRectangle(cornerRadius: 6, style: .continuous)
            .fill(Color.stenoSecondary)
        )
        .overlay(
          RoundedRectangle(cornerRadius: 6, style: .continuous)
            .strokeBorder(Color.stenoBorder, lineWidth: Theme.Space.hairline))
      VStack(alignment: .leading, spacing: 1) {
        Text(section.title)
          .font(.steno(Theme.TextSize.xs, weight: .medium))
          .foregroundStyle(Color.stenoForeground)
        if let subtitle {
          Text(subtitle)
            .font(.steno(Theme.TextSize.xxxs))
            .foregroundStyle(Color.stenoFaint)
            .lineLimit(1)
        }
      }
    }
    .padding(.vertical, 2)
    .accessibilityElement(children: .combine)
    .accessibilityIdentifier("settings-\(section.rawValue)")
  }
}
