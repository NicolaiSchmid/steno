import StenoSpeech
import SwiftUI

/// The speech models and libraries Steno ships with, and their licences.
/// This is where attribution lives, so the sections themselves can stay
/// free of licence strings and repository names.
struct AcknowledgementsView: View {
  struct Library: Identifiable {
    let name: String
    let licence: String
    let url: String
    var id: String { name }
  }

  static let libraries: [Library] = [
    Library(name: "Sparkle", licence: "MIT", url: "https://github.com/sparkle-project/Sparkle"),
    Library(name: "GRDB.swift", licence: "MIT", url: "https://github.com/groue/GRDB.swift"),
    Library(
      name: "FluidAudio", licence: "Apache-2.0",
      url: "https://github.com/FluidInference/FluidAudio"),
    Library(name: "WhisperKit", licence: "MIT", url: "https://github.com/argmaxinc/WhisperKit"),
    Library(name: "SwiftNIO", licence: "Apache-2.0", url: "https://github.com/apple/swift-nio"),
    Library(
      name: "Swift Crypto", licence: "Apache-2.0", url: "https://github.com/apple/swift-crypto"),
    Library(name: "Speex", licence: "BSD", url: "https://github.com/sbooth/CSpeex"),
  ]

  let dismiss: () -> Void

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.lg) {
      Text("Acknowledgements")
        .font(.steno(Theme.TextSize.lg, weight: .semibold))
        .foregroundStyle(Color.stenoStrong)
      ScrollView {
        VStack(alignment: .leading, spacing: Theme.Space.lg) {
          VStack(alignment: .leading, spacing: Theme.Space.sm) {
            SectionLabel(text: "Speech models")
            ForEach(ModelAsset.allCases, id: \.self) { asset in
              row(title: asset.displayName, licence: asset.licence, source: asset.sourceRepo)
            }
          }
          VStack(alignment: .leading, spacing: Theme.Space.sm) {
            SectionLabel(text: "Libraries")
            ForEach(Self.libraries) { library in
              row(title: library.name, licence: library.licence, source: library.url)
            }
          }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
      }
      HStack {
        Spacer()
        Button("Done") { dismiss() }
          .buttonStyle(StenoPrimaryButtonStyle())
          .keyboardShortcut(.defaultAction)
      }
    }
    .padding(Theme.Space.xl)
    .frame(width: 480, height: 440)
    .background(Color.stenoBackground)
  }

  private func row(title: String, licence: String, source: String) -> some View {
    VStack(alignment: .leading, spacing: 2) {
      HStack {
        Text(title)
          .font(.steno(Theme.TextSize.sm))
          .foregroundStyle(Color.stenoForeground)
        Spacer()
        Text(licence)
          .font(.steno(Theme.TextSize.xxs))
          .foregroundStyle(Color.stenoFaint)
      }
      Text(source)
        .font(.steno(Theme.TextSize.xxs).monospaced())
        .foregroundStyle(Color.stenoFaint)
        .textSelection(.enabled)
    }
  }
}
