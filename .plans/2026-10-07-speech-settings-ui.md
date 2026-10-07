# Speech settings in the Settings window

Status: proposal, not implemented. Closes the wording half of the open item "The speech
settings (`onnxSidecarOnMac`, `directmlOnWindows`, `modelsMirror`) ..." under "Open after
the port" in `.plans/2026-10-02-rust-core-and-tauri-shell.md`; that item stays open until
the UI ships.

## Today

The three speech settings (`SpeechSettings` in `crates/steno-speech/src/runtime.rs`) live
only in `speech.json` in the support directory, which nothing writes, and the bridge has no
field for them, so the Settings window shows none. `steno-services` reads the file once at
launch (`steno_services::speech::speech_settings`); the speech-stack plan
(`.plans/2026-10-01-cross-platform-speech-stack.md`, decisions 4 and 5, gate G4) says what
each does:

| Setting | What it does | Platform |
|---|---|---|
| `onnxSidecarOnMac` | Runs Parakeet on the processor in a separate process instead of on the Neural Engine in the app; needs the 2.6 GB full-precision model | Mac |
| `directmlOnWindows` | Runs the speech encoder on a DirectX 12 graphics card, falling back to the processor; off until gate G4 is measured | Windows |
| `modelsMirror` | Fetches the speech models from another server instead of their hosts | All |

## Proposal

All three go into the existing **Transcription** section
(`apps/macos/web/src/windows/settings/transcription-section.tsx`), in a new card under
"On this {computer}". Each row appears only when the host sends its field, so the Swift
host, which has none of them, shows nothing new.

Labels and descriptions follow the Settings rule: no library, runtime or service names, no
"mirror", no "process". Technical facts, such as the graphics card's DirectX version or the
Neural Engine, go behind a Details disclosure (the `Disclosure` component the section
already uses for download failures), which may name them.

### The card for the platform's switch

Each platform shows at most one of the two switches, so the card takes its title from the
switch it holds: "If transcription has trouble" on the Mac, "Speed" on Windows.

**Mac only: compatibility mode** (`onnxSidecarOnMac`), a switch, off by default.

- Label: **Compatibility mode**
- Description: "Turn this on if transcriptions fail or stop partway on this Mac.
  Transcribing takes longer, and a 2.6 GB download is needed first."
- When on and the model is missing, the "On this Mac" card shows the larger model's row
  with its Download button (the row already follows the setting,
  `with_the_sidecar_chosen_parakeet_v3_is_the_onnx_export`).
- Details: "Steno normally transcribes on the Mac's Neural Engine. In compatibility mode it
  uses the processor and a full-precision copy of the same speech model, in a separate
  helper that cannot take Steno down with it."

**Windows only, and only once gate G4 passes: graphics card** (`directmlOnWindows`), a
switch, off by default until G4 decides the default.

- Label: **Use the graphics card**
- Description: "Transcribes faster on most PCs with a recent graphics card. If the card has
  a problem, Steno goes back to the processor on its own."
- State after a fallback in this run: a callout under the row, "The graphics card stopped
  working. Steno uses the processor until it restarts. If a meeting was being transcribed
  then, transcribe it again." (the process-wide switch `directml_switched_off` in
  `crates/steno-speech/src/sidecar/client.rs`, `SpeechSettings::directml_on_windows`). The
  switch cannot tell which case turned it off: a driver that aborts mid-run takes that
  job with it, while a failure in the probe or an overrun between requests costs none.
- Details: "Only the first step of speech recognition runs on the graphics card; the rest
  stays on the processor. Needs a graphics card that supports DirectX 12."

### Disclosure "Advanced" at the foot of the section

**Download source** (`modelsMirror`), all platforms.

- Label: **Download models from**
- Control: a select, "Usual source (recommended)" or "Another server…"; the second shows
  a text field labelled **Server address** with the placeholder `https://`.
- Description: "Only change this if your organisation keeps its own copy of the models."
- Validation line: "Enter an address that starts with https://." Today
  `ModelStore::with_mirror` takes any string unchecked; open question 3.
- Details: "Steno checks every file it downloads against the copy it expects, so another
  server cannot change what is installed." (Checksums and sizes come from the manifest.)
- Today the source covers the speech models only: the speaker recognition models keep
  their own hosts (`SpeechSettings::models_mirror`). Once the diarizer's models move onto
  the shared model store (the "One model store" item under "Speech" in
  `.plans/2026-10-02-rust-core-and-tauri-shell.md`), the source covers them as well, and
  the label and Details above already fit.

### When a change applies

`build()` reads the speech settings once, and `SpeechEngines` keeps the sidecar engine
built from them for the app's run, which is what keeps one speech sidecar at a time
(`crates/steno-services/src/speech.rs`). A change therefore applies at the next start:
while a stored value differs from the running one, the section shows "Takes effect when
Steno restarts." and a **Restart Steno** button. Applying it live would mean swapping
`SpeechEngines` once no job runs; not proposed.

## Bridge fields (names only)

In `TranscriptionSettingsSnapshot` (`crates/steno-bridge/src/settings.rs`,
`Sources/StenoBridge/SettingsSnapshots.swift`), each optional and omitted when the
platform has no such setting:

- `compatibilityModeEnabled: Bool?` (Mac only)
- `graphicsCardEnabled: Bool?` (Windows only, once G4 passes) and
  `graphicsCardSwitchedOff: Bool?`
- `downloadSource: String?` (empty for the default) and `downloadSourceError: String?`
- `needsRestart: Bool?`

Methods (`BridgeMethod` in `crates/steno-bridge/src/envelope.rs`), with the existing
param types:

- `settings.transcription.setCompatibilityModeEnabled` (`SetBoolParams`)
- `settings.transcription.setGraphicsCardEnabled` (`SetBoolParams`)
- `settings.transcription.setDownloadSource` (`SetStringParams`, empty for the default)
- `app.restart` (no params, Rust host only; the shell runs `App::shutdown` and relaunches
  as an update's relaunch does, `tauri::RESTART_EXIT_CODE` in
  `apps/desktop/src-tauri/src/main.rs`)

The host writes `speech.json` atomically (`steno_pipeline::files::replace_file`); the
fixtures under `apps/macos/web/fixtures/bridge/` get one recorded snapshot per platform.

## Open questions

1. Should the download source appear at all, or stay a configuration file and an
   environment variable for the few who need it?
2. "Compatibility mode" or a plainer label such as "Transcribe more slowly but more
   reliably"?
3. Should a download source over plain http be allowed for an address on the local
   network, or https only?
4. Restart button, or only the note that the change applies at the next start?
5. Once G4 passes, should the graphics card be on by default on Windows, which would make
   the switch an opt-out?
