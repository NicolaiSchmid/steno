import StenoCore
import SwiftUI

/// The one editable text: free notes typed by the user, saved once after a
/// short pause and flushed when the view goes away.
struct ScratchpadTab: View {
  let model: MeetingDetailViewModel
  @State private var text = ""
  @State private var loadedFor: UUID?

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.sm) {
      TextEditor(text: $text)
        .font(.steno(Theme.TextSize.sm))
        .foregroundStyle(Color.stenoForeground)
        .scrollContentBackground(.hidden)
        .padding(Theme.Space.sm)
        .background(
          RoundedRectangle(cornerRadius: Theme.Space.radius, style: .continuous)
            .fill(Color.stenoCard))
        .overlay(
          RoundedRectangle(cornerRadius: Theme.Space.radius, style: .continuous)
            .strokeBorder(Color.stenoBorder, lineWidth: Theme.Space.hairline))
        .accessibilityIdentifier("scratchpad-editor")
        .onChange(of: text) { _, newValue in
          guard loadedFor == model.id else { return }
          if newValue != model.meeting?.scratchpad { model.saveScratchpad(newValue) }
        }
      Text("Saved with the meeting and exported into the folder note.")
        .font(.steno(Theme.TextSize.xxs))
        .foregroundStyle(Color.stenoFaint)
    }
    .padding(Theme.Space.lg)
    .onAppear { load() }
    .onChange(of: model.export == nil) { _, _ in load() }
    .onDisappear { Task { await model.flushScratchpad() } }
  }

  private func load() {
    guard loadedFor != model.id else { return }
    text = model.meeting?.scratchpad ?? ""
    loadedFor = model.id
  }
}
