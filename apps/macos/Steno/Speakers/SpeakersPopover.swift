import StenoCore
import SwiftUI

/// One row per speaker in stable cluster order: avatar, the picker, the
/// excerpt while unconfirmed, and Play when the clip file exists. Rows never
/// regroup when a speaker is named; the store observation just redraws them.
/// No footer: every choice already applied.
struct SpeakersPopover: View {
  let model: SpeakersViewModel
  @State private var expanded: UUID?

  static let width: CGFloat = 360
  static let maxListHeight: CGFloat = 360

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.md) {
      Text("Speakers")
        .font(.steno(Theme.TextSize.base, weight: .semibold))
        .foregroundStyle(Color.stenoStrong)
      if let error = model.error { MessageRow(kind: .error, text: error) }
      ScrollView {
        VStack(alignment: .leading, spacing: Theme.Space.md) {
          ForEach(model.rows) { row in
            speakerRow(row)
          }
        }
      }
      .frame(height: listHeight)
    }
    .padding(Theme.Space.lg)
    .frame(width: Self.width)
    .background(Color.stenoPopover)
    .onDisappear { model.stopPlayback() }
  }

  /// Roughly one row's height per speaker plus room for an open option list,
  /// capped; a `ScrollView` inside a popover needs an explicit height.
  private var listHeight: CGFloat {
    let rows = CGFloat(max(model.rows.count, 1)) * 64
    return min(Self.maxListHeight, rows + (expanded == nil ? 0 : 200))
  }

  private func speakerRow(_ row: SpeakersViewModel.Row) -> some View {
    let isPlaying = model.playing == row.id
    return HStack(alignment: .top, spacing: Theme.Space.sm) {
      Avatar(name: row.isConfirmed ? row.person?.displayName : nil, size: 28)
      VStack(alignment: .leading, spacing: Theme.Space.xs) {
        SpeakerPicker(
          model: model, speakerID: row.id,
          isExpanded: Binding(
            get: { expanded == row.id },
            set: { open in
              if open {
                expanded = row.id
              } else if expanded == row.id {
                expanded = nil
              }
            }))
        if !row.isConfirmed, !row.excerpt.isEmpty {
          Text(row.excerpt)
            .font(.steno(Theme.TextSize.xs))
            .foregroundStyle(Color.stenoFaint)
            .lineLimit(2)
            .fixedSize(horizontal: false, vertical: true)
        }
      }
      Spacer(minLength: 0)
      if row.canPlay {
        Button {
          if isPlaying { model.stopPlayback() } else { model.play(row.id) }
        } label: {
          Image(systemName: isPlaying ? "stop.fill" : "play.fill")
            .font(.system(size: 11, weight: .semibold))
            .foregroundStyle(Color.stenoStrong)
            .frame(width: 28, height: 28)
            .background(Circle().fill(Color.stenoSecondary))
        }
        .buttonStyle(.plain)
        .help(isPlaying ? "Stop" : "Play the sample")
        .accessibilityLabel(isPlaying ? "Stop sample" : "Play sample of \(row.displayName)")
        .accessibilityIdentifier("speaker-play-\(row.id.uuidString)")
      }
    }
    .accessibilityElement(children: .contain)
    .accessibilityIdentifier("speaker-row-\(row.id.uuidString)")
  }
}
