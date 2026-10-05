# Record from devices that will not run at 48 kHz

Status: implemented 2026-10-05, in the pull request that adds this file.

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
   for the rate to settle. If the device keeps another rate between 8 and 192 kHz,
   the backend starts at that rate and reports it in `CaptureStream.sampleRate`.
   Anything outside that range fails with `unsupportedSampleRate`, which replaces
   `sampleRateMismatch`.
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
   loop still allocates nothing.
4. **The session converts every device-rate quantity.** The far-end delay
   (latencies are in device frames, rounded down because over-delaying is what the
   echo filter cannot recover from), the relay backlog before a resume, and the
   undrained and dropped ring samples at the end are all counted in 48 kHz frames.
5. **No setting and no new user-facing text.** Recording just works with the
   headset, at the quality the headset delivers.

## Tests

- Converter: chunked input matches one pass over the whole signal; a tone keeps its
  frequency and level from 16, 24, 44.1 and 96 kHz; the output length follows the
  ratio.
- Processing: the drain at 24 kHz allocates nothing (the real-time allocation
  test).
- Session over the synthetic backend at 24 kHz: a three-second recording is three
  seconds of 48 kHz master and 16 kHz sidecars, with the tones intact. A device
  change that restarts at 24 kHz resumes instead of ending the recording.
- Manual, on a Mac: AirPods in a call, start a recording, then switch a running
  recording into a call.
