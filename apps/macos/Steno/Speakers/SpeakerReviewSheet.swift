import StenoCore
import SwiftUI

/// One card per unresolved speaker: play the clip, accept the suggestion,
/// pick a known person or a calendar attendee, type a name, merge into
/// another speaker of this meeting, or skip. Done re-exports once.
struct SpeakerReviewSheet: View {
  @State var model: SpeakerReviewViewModel
  let onFinish: () -> Void
  @State private var names: [UUID: String] = [:]
  @State private var mergeTargets: [UUID: UUID] = [:]

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.md) {
      HStack {
        Text("Who is speaking?")
          .font(.steno(Theme.TextSize.lg, weight: .semibold))
          .foregroundStyle(Color.stenoStrong)
        Spacer()
        Text("\(model.unresolved.count) to review")
          .font(.steno(Theme.TextSize.xs))
          .foregroundStyle(Color.stenoFaint)
      }
      if let error = model.error { MessageRow(kind: .error, text: error) }
      ScrollView {
        VStack(spacing: Theme.Space.md) {
          ForEach(model.unresolved) { card in
            speakerCard(card)
          }
          if model.isDone {
            Text("Every speaker is named. Done re-exports the meeting with the names.")
              .font(.steno(Theme.TextSize.sm))
              .foregroundStyle(Color.stenoMutedForeground)
              .padding(Theme.Space.lg)
          }
        }
      }
      HStack {
        Spacer()
        Button("Later") {
          model.stopPlayback()
          onFinish()
        }
        .buttonStyle(StenoSecondaryButtonStyle())
        .keyboardShortcut(.cancelAction)
        Button("Done") {
          Task {
            await model.finish()
            onFinish()
          }
        }
        .buttonStyle(StenoPrimaryButtonStyle())
        .keyboardShortcut(.defaultAction)
      }
    }
    .padding(Theme.Space.lg)
    .frame(width: 560, height: 520)
    .background(Color.stenoBackground)
    .task { await model.load() }
  }

  private func speakerCard(_ card: SpeakerReviewViewModel.Card) -> some View {
    Card {
      VStack(alignment: .leading, spacing: Theme.Space.sm) {
        HStack(spacing: Theme.Space.sm) {
          Button {
            if model.playing == card.id { model.stopPlayback() } else { model.play(card.id) }
          } label: {
            Image(systemName: model.playing == card.id ? "stop.circle.fill" : "play.circle.fill")
              .font(.system(size: 22))
              .foregroundStyle(card.clipURL == nil ? Color.stenoGhost : Color.stenoStrong)
          }
          .buttonStyle(.plain)
          .disabled(card.clipURL == nil)
          .help(card.clipURL == nil ? "No sample clip" : "Play the ten-second sample")
          VStack(alignment: .leading, spacing: 2) {
            Text(card.speaker.clusterLabel)
              .font(.steno(Theme.TextSize.sm, weight: .semibold))
              .foregroundStyle(Color.stenoStrong)
            if let range = card.speaker.sampleClipRange {
              Text(
                "\(range.lowerBound.timestampText) – \(range.upperBound.timestampText) · confidence \(Int(card.speaker.clusterConfidence * 100))%"
              )
              .font(.steno(Theme.TextSize.xxs))
              .foregroundStyle(Color.stenoFaint)
            }
          }
          Spacer()
          Button("Skip") { model.skip(card.id) }
            .buttonStyle(.plain)
            .font(.steno(Theme.TextSize.xxs))
            .foregroundStyle(Color.stenoFaint)
        }

        if let suggestion = model.suggestion(for: card) {
          HStack(spacing: Theme.Space.sm) {
            Text("Sounds like \(suggestion.person.displayName)")
              .font(.steno(Theme.TextSize.xs))
              .foregroundStyle(Color.stenoForeground)
            StatusChip(text: "\(Int(suggestion.similarity * 100))%", color: Color.stenoLive)
            Button("Accept") { Task { await model.acceptSuggestion(card.id) } }
              .buttonStyle(StenoPrimaryButtonStyle())
          }
        }

        if !card.candidates.isEmpty || !model.attendees.isEmpty {
          ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: Theme.Space.xs) {
              ForEach(card.candidates, id: \.person.id) { match in
                Button("\(match.person.displayName) \(Int(match.similarity * 100))%") {
                  Task { await model.assign(card.id, person: match.person) }
                }
                .buttonStyle(StenoSecondaryButtonStyle())
              }
              ForEach(model.attendees) { attendee in
                Button(attendee.displayName) {
                  Task { await model.assign(card.id, attendee: attendee) }
                }
                .buttonStyle(StenoSecondaryButtonStyle())
              }
            }
          }
        }

        HStack(spacing: Theme.Space.sm) {
          TextField(
            "Name",
            text: Binding(
              get: { names[card.id] ?? "" }, set: { names[card.id] = $0 }))
            .textFieldStyle(.roundedBorder)
            .onSubmit { Task { await model.name(card.id, names[card.id] ?? "") } }
          Button("Name") { Task { await model.name(card.id, names[card.id] ?? "") } }
            .buttonStyle(StenoSecondaryButtonStyle())
            .disabled((names[card.id] ?? "").trimmingCharacters(in: .whitespaces).isEmpty)
        }

        let others = model.allSpeakers.filter { $0.id != card.id }
        if !others.isEmpty {
          HStack(spacing: Theme.Space.sm) {
            Text("Same voice as")
              .font(.steno(Theme.TextSize.xxs))
              .foregroundStyle(Color.stenoFaint)
            Picker(
              "Same voice as",
              selection: Binding(
                get: { mergeTargets[card.id] ?? others.first?.id },
                set: { mergeTargets[card.id] = $0 })
            ) {
              ForEach(others) { other in
                Text(model.displayName(other)).tag(Optional(other.id))
              }
            }
            .labelsHidden()
            .pickerStyle(.menu)
            .frame(width: 180)
            Button("Merge") {
              if let target = mergeTargets[card.id] ?? others.first?.id {
                Task { await model.mergeSpeakers(card.id, into: target) }
              }
            }
            .buttonStyle(StenoSecondaryButtonStyle())
          }
        }
      }
    }
  }
}
