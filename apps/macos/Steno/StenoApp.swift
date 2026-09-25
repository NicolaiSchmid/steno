import SwiftUI

/// Step 1 skeleton: a main window and a menu bar item, enough for the
/// project to build and for the audio workstream's permission spike to run
/// from a bundled, signed app. The scenes fill in over the following steps.
@main
struct StenoApp: App {
  var body: some Scene {
    Window("Steno", id: "main") {
      Text("Steno")
        .frame(minWidth: 480, minHeight: 320)
    }

    MenuBarExtra("Steno", systemImage: "waveform") {
      Button("Open Steno") {
        NSApp.activate()
      }
      Divider()
      Button("Quit Steno") {
        NSApp.terminate(nil)
      }
    }
  }
}
