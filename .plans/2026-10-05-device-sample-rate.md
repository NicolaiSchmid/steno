# Record from devices that will not run at 48 kHz

Status: implemented 2026-10-05 in #198, row A6 of
[`2026-10-07-stable-promotion.md`](2026-10-07-stable-promotion.md) (part of the final
audio path, D9).

Supersedes one decision of [`2026-09-25-audio-capture.md`](2026-09-25-audio-capture.md):
`start()` no longer throws `sampleRateMismatch` when the output device keeps a rate other
than 48 kHz, and the user no longer has to change the device's rate.

## Problem

On the Mac, a recording fails to start with "the audio devices run at 24000 Hz, not
48000 Hz". The capture aggregate takes its clock from the system output device and
asks it for 48 kHz. A Bluetooth headset in the hands-free profile (AirPods while any
app uses their microphone, which a call app does) runs at 24 kHz, or 16 kHz on older
headsets, and refuses. Both macOS backends, Swift
(`Sources/StenoAudio/Capture/LiveCaptureBackend.swift`) and Rust
(`crates/steno-audio/src/capture/live/backend.rs`), then give up.

The same check breaks a recording already running. A headset that switches to the
hands-free profile mid-call reports a rate change, the session rebuilds the backend,
every restart fails the same way, and the recording ends as a lost device.

Windows and Linux never fail here. WASAPI opens both streams with
`AUTOCONVERTPCM`, and PipeWire's adapter converts whatever the graph runs at to the
48 kHz the stream asks for.

## Decisions

1. **The backend accepts the device's rate.** It still asks for 48 kHz and waits
   for the rate to settle (ten reads 20 ms apart). If the device keeps another rate
   between 8 and 192 kHz, the backend starts at that rate and reports it in
   `CaptureStream.sampleRate`. Anything outside that range fails with
   `unsupportedSampleRate`, which replaces `sampleRateMismatch`. Once its listeners
   are registered, the backend reads the rate again: a rate that settled after the
   reads gave up, but before the rate listener existed, is judged as a rate
   notification, so the session rebuilds at the rate the device runs at.
2. **The processing thread converts to 48 kHz before anything else.** The rings
   carry the device's rate. When the stream is not at 48 kHz, the processing thread
   passes each lane through a streaming rate converter (`RateConverter`) before
   framing, echo cancellation, metering and the writer. Everything after the
   converter, the files included, stays at 48 kHz as before. The IOProc is not
   touched.
3. **One converter design in both cores.** A polyphase Kaiser-windowed sinc, the
   same table as `crates/steno-audio/src/codec/sinc.rs` (128 phases, 64 taps,
   cutoff 0.45 of the lower rate, beta 9). The position advances in exact integer
   steps, so there is no drift over hours. The filter is centred, so the lanes keep
   their timing, and every lane gets the same input count, so all lanes produce the
   same number of samples. All buffers are allocated up front, and the processing
   loop still allocates nothing. In Rust the decoder's `SincStream` keeps its own
   loop over the same table: it allocates and keeps a float position, and its
   output must stay what it is.
4. **The session converts every device-rate quantity.** The far-end delay, the
   relay backlog before a resume, and the undrained and dropped ring samples at the
   end are all counted in 48 kHz frames, rounded down (over-delaying is what the
   echo filter cannot recover from). Each latency is read on its own device, in that
   device's frames: the output device is the clock master and runs at the stream's
   rate, while a microphone on another device keeps its own (a 48 kHz built-in
   microphone beside a headset at 24 kHz), so the backend rescales the microphone's
   latency to the stream's rate first. A stop that overtakes a rebuild counts what
   the rings hold at the restarted stream's rate.
5. **No setting.** Recording just works with the headset, at the quality the
   headset delivers. The one changed text is the error for a rate outside the
   range: "the audio devices run at N Hz, which Steno cannot record" replaces "the
   audio devices run at N Hz, not 48000 Hz". An aggregate that is gone reads as
   0 Hz there. The desktop app's status line says the same in its own words:
   "Recording could not start: the audio devices run at N Hz, which Steno
   cannot record."

## Quality bar

D9's bar for the resampler holds here too: within 0.3 dB to 6 kHz, and nothing
aliased into the speech band (less than -60 dB below 8 kHz). A9's 44.1 kHz sweep
proves the offline `SincResampler`, not this loop, so the converter has its own
tests. Measured on 2026-10-08: from 16, 24, 44.1, 96 and 192 kHz the level
stays within 0.01 dB from 100 Hz to 6 kHz, and a 44.1 kHz sweep to 22 kHz leaves
-99.8 dB at most below 8 kHz. From 8 kHz the band is the device's own (-0.38 dB at
3.4 kHz).

## Tests

- Converter (`crates/steno-audio/tests/rate_converter.rs`,
  `Tests/StenoAudioTests/RateConverterTests.swift`):
  - chunked input matches one pass over the whole signal (16 to 96 kHz);
  - a tone keeps its level within 0.1 dB and its frequency within 2 Hz from 8, 16,
    24, 44.1, 96 and 192 kHz, and the output length follows the ratio less half a
    window;
  - a 1 kHz tone from 44.1 kHz leaves a residual under -80 dB once the fitted sine
    is removed, which fails if the adjacent phases are not blended;
  - Rust only: the stream matches the offline `SincResampler` at 24 kHz (whole
    phases) and at 44.1 and 16 kHz (blended phases);
  - a 30 kHz tone at 96 kHz is rejected below -50 dB;
  - an impulse lands on its output sample (200 at 24 kHz, 300 at 16 kHz);
  - reset starts a new signal; the range is 8 to 192 kHz.
- Real-time: the converting processing loop allocates nothing after warm-up at
  24 kHz and at 22.05 kHz, where a frame's worth of device samples is not whole,
  with exact frame counts (`tests/realtime.rs`, `RealTimeAllocationTests.swift`).
- Session over the synthetic backend:
  - a three-second recording at 24 kHz is exactly 299 frames of 48 kHz master and
    its 16 kHz sidecars, with the tones intact, and the far end reaches the
    canceller exactly 10 080 samples late (240 + 4 800 device frames);
  - a device change that restarts at 24 kHz resumes instead of ending the
    recording, with an exact master length;
  - frame counts rescale between rates rounding down;
  - Rust only: a stop that overtakes a rebuild at 24 kHz counts the rings' leftovers
    in 48 kHz frames.
- The backend: a rate read again after the listeners that differs from the
  started rate is judged as a rate notification and reports `sampleRateChanged`
  (`capture/live/backend.rs`, `DeviceSnapshotTests.swift`; macOS only).
- Manual, on a Mac: AirPods in a call, start a recording, then switch a running
  recording into a call.

## Limits

- **The range.** 8 kHz is narrowband hands-free, the lowest rate a headset runs
  at. 192 kHz is the highest a Mac interface offers: four input samples per output
  sample. There the 64 taps leave a transition band about 9 kHz wide, so content
  just above 24 kHz aliases into the top of the band (a 28 kHz tone lands at
  20 kHz, at -49 dB); below 8 kHz nothing aliases above -97 dB, so the bar holds.
- **A late rate costs one rebuild.** A rate that settles after the 200 ms of reads
  is caught by the read after the listeners. The audio until the rebuild (the
  500 ms coalesce delay plus the teardown) goes through the old rate's path, so it
  is mislabelled for that half second. The same holds for every rate change mid
  recording: the notification burst is judged as a whole.
- **Under 11 ms per rebuild across rates.** The old converter's held half window
  and its last partial frame go with the old processing thread (7.3 ms measured per
  rebuild); `dropped_frames` does not count them. Going down in rate, the residue
  of less than a frame the old thread leaves in the rings is read at the new rate.
- **The rings are sized for 48 kHz.** The sink is built before the device's rate is
  known, for two seconds at 48 kHz (131 072 samples per lane). That is 5.5 s at
  24 kHz but 1.37 s at 96 kHz and 0.68 s at 192 kHz. A writer stalled during a
  rebuild overruns them sooner at those rates; the overrun is counted in
  `dropped_frames`. Main refused such devices, so it is not a regression.
- **A rate the converter cannot take.** The live backends refuse it at `start`. A
  backend that reports one anyway is recorded unconverted rather than crashing the
  session (Rust logs it).
