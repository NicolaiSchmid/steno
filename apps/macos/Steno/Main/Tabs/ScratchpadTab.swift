import StenoCore
import SwiftUI

/// The one editable text: free notes typed by the user, saved once after a
/// short pause and flushed when the view goes away. The editor is the
/// clearest raised surface on the page (radius 12, hairline, 12 pt inset,
/// at least 240 pt tall); it stays editable in every meeting state, notes
/// during a call included. While the meeting is queued or processing the
/// card sits above the editor.
struct ScratchpadTab: View {
  let model: MeetingDetailViewModel
  /// Where the pipeline is with this meeting while it is queued or
  /// processing, from `controller.progress.entry(for:)`; nil otherwise.
  let progress: ProcessingProgressModel.Entry?
  @State private var text = ""

  /// The editor never collapses below this, so an empty scratchpad still
  /// reads as a place to type.
  static let minimumEditorHeight: CGFloat = 240

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.sm) {
      ProcessingCardSlot(progress: progress, meeting: model.meeting)
        .padding(.bottom, Theme.Space.sm)
      TextEditor(text: $text)
        .font(.steno(Theme.TextSize.sm))
        .proseLeading()
        .foregroundStyle(Color.stenoForeground)
        .scrollContentBackground(.hidden)
        .padding(Theme.Space.md)
        .frame(minHeight: Self.minimumEditorHeight)
        .background(Theme.Radius.lg.shape.fill(Color.stenoRaised))
        .overlay(Theme.Radius.lg.shape.hairline())
        .accessibilityIdentifier("scratchpad-editor")
        .onChange(of: text) { _, newValue in
          if newValue != model.meeting?.scratchpad { model.saveScratchpad(newValue) }
        }
      Text("Saved with the meeting and exported into the folder note.")
        .font(.steno(Theme.TextSize.xxs))
        .foregroundStyle(Color.stenoFaint)
        .accessibilityIdentifier("scratchpad-hint")
    }
    .readingColumn()
    .onAppear { text = model.meeting?.scratchpad ?? "" }
    .onDisappear { Task { await model.flushScratchpad() } }
  }
}
