import StenoCore
import SwiftUI

/// One select for one speaker: a button showing the confirmed name (or
/// "Name this speaker…") that expands into a search-or-create field with the
/// ranked options under it. Return commits the highlighted option, Down and
/// Up move it, Escape collapses the field. `select` runs on Return or click
/// only, never on a keystroke. The keyboard rules live in
/// `SpeakerPickerState`.
struct SpeakerPicker: View {
  let model: SpeakersViewModel
  let speakerID: UUID
  @Binding var isExpanded: Bool
  @State private var state = SpeakerPickerState()
  @State private var hovering = false
  @FocusState private var fieldFocused: Bool

  private var row: SpeakersViewModel.Row? { model.row(speakerID) }

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.xs) {
      if isExpanded {
        field
        optionList
      } else {
        trigger
      }
    }
    .onChange(of: isExpanded) { _, expanded in
      if expanded { open() }
    }
    .onChange(of: model.rows) { _, _ in
      if isExpanded { refresh() }
    }
    .onChange(of: model.recent) { _, _ in
      if isExpanded { refresh() }
    }
  }

  private var trigger: some View {
    Button {
      isExpanded = true
    } label: {
      HStack(spacing: Theme.Space.xs) {
        if let row, row.isConfirmed {
          Text(row.displayName)
            .font(.steno(Theme.TextSize.sm, weight: .medium))
            .foregroundStyle(Color.stenoStrong)
          if let email = row.person?.email, !email.isEmpty {
            Text("·").foregroundStyle(Color.stenoGhost)
            Text(email)
              .font(.steno(Theme.TextSize.xxs))
              .foregroundStyle(Color.stenoFaint)
              .lineLimit(1)
          }
        } else {
          Text("Name this speaker…")
            .font(.steno(Theme.TextSize.sm))
            .foregroundStyle(Color.stenoMutedForeground)
        }
        Image(systemName: "chevron.down")
          .font(.system(size: 9, weight: .semibold))
          .foregroundStyle(Color.stenoGhost)
          .opacity(hovering ? 1 : 0)
      }
      .contentShape(Rectangle())
    }
    .buttonStyle(.plain)
    .onHover { hovering = $0 }
    .accessibilityIdentifier("speaker-picker-\(speakerID.uuidString)")
    .accessibilityLabel(
      row.map { $0.isConfirmed ? "Speaker \($0.displayName)" : "Name this speaker" }
        ?? "Name this speaker")
  }

  private var field: some View {
    TextField(
      "Name this speaker…",
      text: Binding(
        get: { state.query },
        set: { text in
          state.queryChanged(text)
          refresh()
        }))
      .textFieldStyle(.plain)
      .font(.steno(Theme.TextSize.sm))
      .padding(.horizontal, Theme.Space.sm)
      .padding(.vertical, Theme.Space.xs + 2)
      .background(
        RoundedRectangle(cornerRadius: Theme.Space.radiusSmall, style: .continuous)
          .fill(Color.stenoSecondary))
      .overlay(
        RoundedRectangle(cornerRadius: Theme.Space.radiusSmall, style: .continuous)
          .strokeBorder(Color.stenoRing, lineWidth: Theme.Space.hairline))
      .focused($fieldFocused)
      .onSubmit { commit() }
      .onKeyPress(.downArrow) {
        state.move(.down)
        return .handled
      }
      .onKeyPress(.upArrow) {
        state.move(.up)
        return .handled
      }
      .onKeyPress(.escape) {
        isExpanded = false
        return .handled
      }
      .accessibilityIdentifier("speaker-field-\(speakerID.uuidString)")
  }

  private var optionList: some View {
    VStack(alignment: .leading, spacing: 0) {
      ForEach(Array(state.options.enumerated()), id: \.element.id) { index, option in
        Button {
          choose(option)
        } label: {
          optionRow(option, highlighted: index == state.highlighted)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier(Self.identifier(for: option))
      }
      if state.options.isEmpty {
        Text("Nobody matches")
          .font(.steno(Theme.TextSize.xxs))
          .foregroundStyle(Color.stenoFaint)
          .padding(.horizontal, Theme.Space.sm)
          .frame(height: 24)
      }
    }
  }

  private func optionRow(_ option: SpeakerOptions.Option, highlighted: Bool) -> some View {
    HStack(spacing: Theme.Space.sm) {
      switch option.kind {
      case .person(let person):
        Avatar(name: person.displayName, size: 18)
      case .create:
        Image(systemName: "plus.circle")
          .font(.system(size: 14))
          .foregroundStyle(Color.stenoFaint)
          .frame(width: 18, height: 18)
      }
      Text(Self.label(for: option))
        .font(.steno(Theme.TextSize.xs))
        .foregroundStyle(Color.stenoStrong)
        .lineLimit(1)
      Spacer(minLength: Theme.Space.sm)
      if let tag = option.tag {
        Text(tag.rawValue)
          .font(.steno(Theme.TextSize.xxs))
          .foregroundStyle(Color.stenoFaint)
      }
    }
    .padding(.horizontal, Theme.Space.sm)
    .frame(height: 24)
    .background(
      RoundedRectangle(cornerRadius: Theme.Space.radiusSmall, style: .continuous)
        .fill(highlighted ? Color.stenoSecondary : Color.clear))
    .contentShape(Rectangle())
  }

  /// `Create “Anna”` for a create row, the person's name otherwise.
  nonisolated static func label(for option: SpeakerOptions.Option) -> String {
    switch option.kind {
    case .person(let person): person.displayName
    case .create(let text): "Create “\(text)”"
    }
  }

  nonisolated static func identifier(for option: SpeakerOptions.Option) -> String {
    switch option.kind {
    case .person(let person): "speaker-option-\(person.id.uuidString)"
    case .create: "speaker-create"
    }
  }

  private func open() {
    state.reset(prefill: model.prefill(for: speakerID))
    refresh()
    fieldFocused = true
  }

  private func refresh() {
    state.setOptions(model.options(for: speakerID, query: state.query))
  }

  private func commit() {
    guard let option = state.commit() else { return }
    choose(option)
  }

  private func choose(_ option: SpeakerOptions.Option) {
    Task {
      await model.select(option, for: speakerID)
      isExpanded = false
    }
  }
}
