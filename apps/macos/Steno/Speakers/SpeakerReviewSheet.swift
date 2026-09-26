import StenoCore
import SwiftUI

/// One card per unresolved speaker: play the clip, accept the suggestion,
/// pick a known person or a calendar attendee, type a name, merge into
/// another speaker of this meeting, or skip. Done re-exports once, and only
/// when something changed.
struct SpeakerReviewSheet: View {
  @Bindable var model: SpeakerReviewViewModel
  let onFinish: () -> Void

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
            Text(doneHint)
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

  private var doneHint: String {
    model.didChange
      ? "Every speaker is named. Done re-exports the meeting with the names."
      : "Every speaker is named. Nothing changed, so Done leaves the export as it is."
  }

  private func speakerCard(_ card: SpeakerReviewViewModel.Card) -> some View {
    let isPlaying = model.playing == card.id
    let draft = (model.draftNames[card.id] ?? "").trimmingCharacters(in: .whitespaces)
    return Card {
      VStack(alignment: .leading, spacing: Theme.Space.sm) {
        HStack(spacing: Theme.Space.sm) {
          Button {
            if isPlaying { model.stopPlayback() } else { model.play(card.id) }
          } label: {
            Image(systemName: isPlaying ? "stop.circle.fill" : "play.circle.fill")
              .font(.system(size: 22))
              .foregroundStyle(card.clipURL == nil ? Color.stenoGhost : Color.stenoStrong)
          }
          .buttonStyle(.plain)
          .disabled(card.clipURL == nil)
          .help(card.clipURL == nil ? "No sample clip" : "Play the ten-second sample")
          .accessibilityLabel(
            card.clipURL == nil
              ? "No sample clip for \(card.speaker.clusterLabel)"
              : isPlaying ? "Stop sample" : "Play sample of \(card.speaker.clusterLabel)")
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
            .accessibilityLabel("Skip \(card.speaker.clusterLabel)")
        }

        if let suggestion = model.suggestion(for: card) {
          HStack(spacing: Theme.Space.sm) {
            Text("Sounds like \(suggestion.person.displayName)")
              .font(.steno(Theme.TextSize.xs))
              .foregroundStyle(Color.stenoForeground)
            StatusChip(text: "\(Int(suggestion.similarity * 100))%", color: Color.stenoLive)
            Button("Accept") { Task { await model.acceptSuggestion(card.id) } }
              .buttonStyle(StenoPrimaryButtonStyle())
              .accessibilityLabel("Accept \(suggestion.person.displayName)")
          }
        }

        if let guess = card.nameSuggestion, let name = guess.name {
          HStack(spacing: Theme.Space.sm) {
            Text("The conversation suggests \(name)")
              .font(.steno(Theme.TextSize.xs))
              .foregroundStyle(Color.stenoForeground)
            StatusChip(text: "\(Int(guess.confidence * 100))%", color: Color.stenoInfo)
            Button("Use") { Task { await model.acceptNameSuggestion(card.id) } }
              .buttonStyle(StenoSecondaryButtonStyle())
              .accessibilityLabel("Name this speaker \(name)")
          }
          .help(guess.evidence)
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
              get: { model.draftNames[card.id] ?? "" },
              set: { model.draftNames[card.id] = $0 }))
            .textFieldStyle(.roundedBorder)
            .onSubmit { Task { await model.nameFromDraft(card.id) } }
          Button("Name") { Task { await model.nameFromDraft(card.id) } }
            .buttonStyle(StenoSecondaryButtonStyle())
            .disabled(draft.isEmpty)
        }

        let others = model.mergeCandidates(for: card)
        if !others.isEmpty {
          HStack(spacing: Theme.Space.sm) {
            Text("Same voice as")
              .font(.steno(Theme.TextSize.xxs))
              .foregroundStyle(Color.stenoFaint)
            Picker(
              "Same voice as",
              selection: Binding(
                get: { model.mergeTargets[card.id] },
                set: { model.mergeTargets[card.id] = $0 })
            ) {
              Text("Choose a speaker").tag(UUID?.none)
              ForEach(others) { other in
                Text(model.displayName(other)).tag(Optional(other.id))
              }
            }
            .labelsHidden()
            .pickerStyle(.menu)
            .frame(width: 180)
            .accessibilityLabel("Same voice as")
            Button("Merge") { Task { await model.mergeIntoChosenTarget(card.id) } }
              .buttonStyle(StenoSecondaryButtonStyle())
              .disabled(model.mergeTargets[card.id] == nil)
          }
        }
      }
    }
  }
}
