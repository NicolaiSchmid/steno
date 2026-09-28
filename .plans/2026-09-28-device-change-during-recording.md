# Steno: device changes during a recording and an honest auto-stop

Status: proposal, 2026-09-28. Triggered by first-run feedback (second round).

Binding context: [`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md) (Capture (Mac)),
[`2026-09-25-audio-capture.md`](2026-09-25-audio-capture.md) (the capture design, its
verified facts table and the "rebuilding mid-meeting is v1.1" deferral this plan lifts),
[`2026-09-25-macos-app-and-release.md`](2026-09-25-macos-app-and-release.md) (recording flow,
detection rules). Siblings from the same feedback round:
[`2026-09-28-first-run-feedback.md`](2026-09-28-first-run-feedback.md) (index, step 6),
[`2026-09-28-floating-recording-indicator.md`](2026-09-28-floating-recording-indicator.md)
(the bubble; its states table references the auto-stop row specified here),
[`2026-09-28-start-recording-from-main-window.md`](2026-09-28-start-recording-from-main-window.md)
(the sidebar control that shows the same countdown). This plan owns the auto-stop behaviour and
copy on every surface. The implementing PR adds one line to the Deferred section of the audio
capture plan pointing here.

## Goal

A recording survives the audio device changes that happen during and at the end of a normal
call (AirPods switching profile, a headset dropping, a USB microphone unplugged and replugged,
the default output moved) and keeps writing to the same files. When the call app closes the
microphone, Steno offers to stop and does so after a visible grace period, and says so on the
meeting afterwards. Whatever ends a recording, the meeting shows why.

## Findings

### The owner's observation

The owner recorded a 56 minute call in Zen (a browser). When the call ended the recording
stopped on its own; the main window showed the meeting in Processing with a duration of 56:13
and no reason. The owner read this as a feature ("it auto-stops when the meeting mic
permission stops"). It is not one.

### What the code does

- Detection does not stop recordings. `apps/macos/Steno/Detection/DetectionController.swift:69-88`
  handles `.microphoneReleased` by dismissing the prompt and nothing else; `.microphoneOpened`
  while recording is dropped. The detector (`Sources/StenoAudio/Detection/MeetingDetector.swift`)
  only emits events; it never touches the recorder.
- The recording ended through the device-loss path.
  `Sources/StenoAudio/Capture/LiveCaptureBackend.swift:136-155` registers property listeners on
  `kAudioHardwarePropertyDefaultSystemOutputDevice`, `kAudioHardwarePropertyDefaultOutputDevice`,
  `kAudioDevicePropertyDeviceIsAlive` of the output device, `DeviceIsAlive` of the microphone
  and, because `Settings.inputDeviceUID` is nil by default,
  `kAudioHardwarePropertyDefaultInputDevice`. Every one of them calls the same closure,
  `sink.reportDeviceLost()`. No listener distinguishes "changed" from "gone", and none checks
  whether the resolved devices actually differ.
- `Sources/StenoAudio/RealTime/LaneFrameSink.swift:71-77` latches the first report and runs the
  handler once; `Sources/StenoAudio/Capture/CaptureSession.swift:274-281` (`deviceLost()`) moves
  to `.stopping`, calls `finish()` (backend off, threads stopped, files closed) and ends in
  `.failed(.deviceLost, recording: partial)`.
- `apps/macos/Steno/Recording/RecordingController.swift:176-187` sees `.failed`, sets
  `lastError = "Recording failed: an audio device disappeared"` and calls `stop()`. `stop()`
  (lines 126-160) receives the partial result, hands it to `LocalRecordingIntake.complete`,
  which writes the duration, sets the state to `.queued` and enqueues; then it sets
  `lastWarning = "An audio device disappeared; the partial recording was kept."`. The pipeline
  then processes the meeting like any other. `apps/macos/StenoTests/RecordingControllerTests.swift:140-173`
  pins exactly this: after a synthetic device loss the stored meeting is `.ready`.
- The only place the reason is visible is the menu bar popover
  (`apps/macos/Steno/MenuBar/MenuBarView.swift:25-33`, two `MessageRow`s, one error and one
  warning, both cleared by the next start). The meeting row stores nothing:
  `Sources/StenoCore/Storage/LocalRecordingIntake.swift:128-155` writes only the duration, and
  `apps/macos/Steno/Main/MeetingDetailView.swift:67-69` shows a reason only for `.failed`.

So the chain behind the screenshot is: a HAL notification at call end, `.failed(.deviceLost)`
carrying 56:13 of audio, `complete` enqueuing it, Processing in the window, and the only
explanation two lines in a popover the owner did not open.

### Which listener fired

The code cannot tell (all five share one closure), and the audio capture plan's `[manual]`
checks for device changes were never run (the workstream had no Mac), so the owner's call is
the first field observation. From the platform behaviour:

- With a Bluetooth headset, ending a call is the moment macOS leaves the HFP profile
  (`coreaudiod: Transitioning out of eSCO active HFP IO`) and the headset's input goes away
  or is re-registered. That fires `kAudioHardwarePropertyDefaultInputDevice` (the default input
  moves to the built-in microphone or is re-announced for the same headset), often
  `kAudioHardwarePropertyDefaultOutputDevice` as the device re-registers, and, when the HAL
  recreates the device object, `DeviceIsAlive` on the old object. Any one of these is enough.
- With wired devices, the default devices do not change at call end, so a recording over the
  built-in microphone and speakers would not have stopped; the owner should confirm which
  devices were in use (Open questions).
- A property listener fires when the property changed, including a change back to the same
  value after a transient. The listener does not carry the new value; Steno never re-reads it.
- The tap is not the trigger. The tap is global except Steno
  (`Sources/StenoAudio/Capture/ProcessTap.swift`, `stereoGlobalTapButExcludeProcesses`); the
  process list changing does not affect it, and a tapped process that stops playing or exits
  leaves the aggregate and the IOProc running and delivering silence. No listener on
  `kAudioHardwarePropertyProcessObjectList` exists in the backend, so Zen closing the call
  could not have reached `reportDeviceLost` through the tap.

Every plausible trigger is therefore a default-device or device-alive notification in
`LiveCaptureBackend.swift:136-155`, and the sample-rate change that comes with a Bluetooth
profile switch is a second, unwatched hazard: `kAudioDevicePropertyNominalSampleRate` is read
once at start (`LiveCaptureBackend.swift:105-116`) and never again, so a profile switch that
does not fire a default-device notification leaves the aggregate at a rate the master is not
labelled with, or the tap delivering zeros.

### What a rebuild has to respect

- Real time. The IOProc (`Sources/StenoAudio/RealTime/IOProcRunner.swift:57-90`) and the
  processing thread (`Sources/StenoAudio/RealTime/ProcessingThread.swift`) allocate nothing and
  take no locks; `LaneFrameSink` is single-producer. The listeners run on the backend's serial
  `listenerQueue`, not the IO thread, and today's report reaches the session through
  `Task { await self?.deviceLost() }`, so teardown already runs on the actor and never inside a
  HAL callback (which practitioners report deadlocks against the reconfiguration that caused
  it). The rebuild keeps that shape.
- The far-end delay. `CaptureStream.inputLatencyFrames` and `outputLatencyFrames` are read
  once from the devices at start (`LiveCaptureBackend.swift:161-171`) and turned into a fixed
  delay line at `ProcessingThread` construction (`ProcessingThread.swift:75-81`,
  `CaptureSession.swift:138-149`). New devices mean a new delay and a cold echo canceller.
- The writer. `RecordingWriter` files are opened once and the asset URLs are fixed at start
  (`CaptureSession.swift:158-166`, `finish()` at 236-272); `WriterThread` drains `FrameRelay`
  independently of who produces. Nothing in the writer cares whether the producer above it was
  replaced.
- Swift 6. `CaptureSession` is an actor; `LiveCaptureBackend` is `@unchecked Sendable` behind
  an `NSLock`; `start` blocks for up to a second in `NominalSampleRate.settle`. A rebuild on
  the actor blocks the same way the initial start does today.
- Verified facts from the capture plan that carry over: the aggregate keys, the tap description
  and `TapAutoStart` are public and verified against Apple docs; `NominalSampleRate` on an
  aggregate is unverified; there is no permission status API. Nothing here needs a new API.

## Non-goals

- Per-application taps, a tap on Zen alone, or ScreenCaptureKit capture.
- Two assets per meeting or stitching segments in the pipeline (the asset stays one master
  with sidecars).
- Changing the default device on the user's behalf, or pinning the recording to the device it
  started on when the user moves output elsewhere.
- A silent-tap watchdog that rebuilds when the tap delivers zeros while the system plays
  (Open questions; the rebuild path built here is what such a watchdog would call).
- Auto-stopping in-person recordings, or stopping on calendar event end.
- A setting that turns the auto-stop off (open question). The countdown is cancellable.
- Persisting device-change counts on the meeting. They stay in `CaptureStatistics` and the CLI.
- iOS.

## Decisions

1. **Survive device changes by rebuilding in place.** A watched notification no longer ends
   the recording. The session stops the HAL objects, starts the backend again on the devices
   as they are now, replaces the processing thread with one built for the new latencies, and
   keeps the sink, the relay, the writer thread, the writer and the files. The recording ends
   with `.deviceLost` only when the restart fails after bounded retries. Reasons: the owner's
   case is a device that changes, not one that is gone; the writer and the asset are already
   indifferent to the producer; one asset per meeting keeps the pipeline and the schema as they
   are. Ending and restarting a segment would need a second asset per meeting and a stitching
   stage for a gap the user never asked to see.
2. **Rebuild only when something actually changed.** The backend's listener, on its own queue
   and after a 500 ms coalescing delay, resolves the default devices again and compares: same
   output UID, same microphone UID, both alive, aggregate still at 48 kHz means nothing to do
   (logged as an ignored notification). Otherwise it reports a `DeviceChange` with the reason
   (`defaultOutputChanged`, `defaultInputChanged`, `outputDeviceGone`, `inputDeviceGone`,
   `sampleRateChanged`). Reason: a Bluetooth transition produces several notifications in a
   burst and some of them change nothing; rebuilding on each would re-open the microphone and
   can itself re-trigger HFP negotiation.
3. **Also watch the aggregate's nominal sample rate.** `kAudioDevicePropertyNominalSampleRate`
   on the aggregate joins the watched list; a rate other than 48 kHz is a `sampleRateChanged`
   change and rebuilds. Reason: the unwatched hazard above.
4. **The session, not the backend, orchestrates the rebuild.** `CaptureBackend` keeps `start`
   and `stop`; the session calls them again. Reason: the retry policy, the gap accounting and
   the processing thread swap belong with the state machine, and `SyntheticCaptureBackend`
   then exercises the exact production path on CI by reporting a change and accepting a
   second `start`.
5. **Retries: 4 attempts over about 4 s, then `.deviceLost`.** Backoff 250 ms, 500 ms, 1 s,
   2 s on the injected clock. Reason: a Bluetooth device is absent for one to two seconds
   while it changes profile; a USB device replugged by hand takes longer and is a loss the
   user can see and restart from. Bounded so a truly missing device does not leave the
   recording spinning.
6. **The gap is filled with silence and counted.** Between the old IOProc's last callback and
   the new backend's start the session writes zeros for the measured wall-clock gap into every
   lane (capped at 10 s) through the sink's producer API, at a moment when no producer runs.
   `CaptureStatistics` gains `deviceChanges` and `gapSeconds`. Reason: the master stays
   aligned to wall time, so transcript timestamps and the elapsed timer agree. The fill is not
   on the real-time path and touches no ring while an IOProc exists.
7. **The state stays `.recording`; a notice stream carries the change.** `CaptureSession`
   gains `notices: AsyncStream<CaptureNotice>` with `.deviceChanged(reason)`,
   `.deviceResumed(attempt:gapSeconds:)` and `.deviceLost`. Reason: a new `CaptureState` case
   would ripple through every `case .recording` guard in the app for a transient of a few
   hundred milliseconds; the app wants a line of text, not a state.
8. **An intentional auto-stop after the call app closes the microphone.** A `.call` recording
   during which a foreign process held the microphone arms a 90 s countdown when the detector
   reports `.microphoneReleased`; `.microphoneOpened` before it elapses cancels it; the user
   can cancel ("Keep recording") or stop at once; when it elapses the recording stops with
   `endReason = .callEnded(appName)`. Never for `.inPerson`, never when no foreign process
   held the microphone during the recording. Reason: this is the feature the owner believed
   existed; 90 s covers a Meet or Zoom reconnect after a dropped connection and costs a minute
   and a half of silence at the end of a call that really ended; the detector's
   `.microphoneReleased` already means "no process but Steno runs input". The policy lives in
   `RecordingController`; the detection controller forwards the events through one closure
   wired by `AppController`, the same way the prompt starts a recording today.
9. **Every recording stores why it ended.** `Meeting.endReason: RecordingEndReason?` with
   `.manual`, `.callEnded(appName: String?)`, `.deviceLost`, `.quit`, written by
   `LocalRecordingIntake.complete` from `RecordingResult.endReason`. Column `endReason` on
   `meeting`, migration `v3`. On the meeting, not the asset: the list and the detail read
   meetings.
10. **Grace period is a constant.** `RecordingController.autoStopGrace = .seconds(90)`. Reason:
    a knob in Settings is not worth the copy; change the constant in a later plan if field use
    says so.

## Behaviour spec

### Capture session on a device notification

| Event | Condition | Session state | Notice | Files |
|---|---|---|---|---|
| Notification, devices resolve identical, alive, 48 kHz | any | `.recording`, unchanged | none (logged "ignored") | untouched |
| `DeviceChange(reason)` reported | `.recording` | stays `.recording`; backend stopped, processing thread stopped, silence written for the gap, backend started, new processing thread started | `.deviceChanged(reason)` then `.deviceResumed(attempt: n, gapSeconds:)` | same master and sidecars, contiguous |
| Restart throws | attempt < 4 | stays `.recording`; wait backoff, try again | none until resolved | untouched during the wait (gap grows) |
| Restart throws | attempt 4 | `.stopping` then `.failed(.deviceLost, recording: partial)`; `endedOnDeviceLoss = true` | `.deviceLost` | finalised as today |
| `DeviceChange` reported | not `.recording` | ignored | none | |
| `stop()` during a rebuild | any attempt | the rebuild is abandoned, `finish()` runs, `.idle` | none | finalised |
| Writer fails during a rebuild | any | `.failed(.writerFailed)` as today | | |

`CaptureStatistics`: `deviceChanges: Int` (successful rebuilds), `gapSeconds: TimeInterval`
(silence written), `endedOnDeviceLoss` unchanged in meaning (true only after the fourth
failure). The echo canceller is reset before the new processing thread starts, as at `start`.
`CaptureSession.stream` returns the new backend's `CaptureStream` after a rebuild.

### The recorder during a call

Foreign microphone activity, forwarded by `DetectionController` while `isRecording`:

| Recorder state | Event | Result |
|---|---|---|
| `.recording`, mode `.call` | `.microphoneOpened(appName)` | `sawForeignMicrophone = true`, `callAppName = appName`; cancel a pending auto-stop if one is armed |
| `.recording`, mode `.call`, `sawForeignMicrophone` | `.microphoneReleased` | `autoStop = AutoStopCountdown(appName, endsAt: now + 90 s)` |
| `.recording`, mode `.call`, not `sawForeignMicrophone` | `.microphoneReleased` | nothing (no call was ever observed) |
| `.recording`, mode `.inPerson` | any | nothing |
| auto-stop armed | `keepRecording()` | `autoStop = nil`; not re-armed until the next `.microphoneOpened` then `.microphoneReleased` |
| auto-stop armed | `stopNow()` or countdown elapsed | `stop(reason: .callEnded(appName))` |
| auto-stop armed | user stops from any surface | `stop(reason: .manual)`, `autoStop = nil` |
| auto-stop armed | device change notice | countdown unaffected |
| any | `stop()` for any reason | `autoStop = nil`, `sawForeignMicrophone = false`, `callAppName = nil` |

The recording started from the detection prompt sets `sawForeignMicrophone` and `callAppName`
at start (the prompt's `appName`). A recording started manually learns them from the first
`.microphoneOpened` seen while recording. The detector's 2 s debounce and 1 s poll mean the
countdown starts within 3 s of the app releasing the microphone.

### Surfaces while the auto-stop is armed

Bubble (`BubblePresentation.make(state:autoStop:)`, the row the floating indicator plan's
states table references):

| Recorder state | Content | Stop button | Body click |
|---|---|---|---|
| `.recording(since:)`, `autoStop != nil` | glyph, bars, elapsed, then a second line `xxs` `mutedForeground`: "<App> closed the mic. Stopping in 1:29" with the prompt's 2 pt countdown hairline along the bottom (`Motion.countdown`), and a "Keep recording" text button before the stop button | enabled, calls `recorder.stop()` (manual) | opens the live meeting |

The bubble widens to fit the line (max 480 like the prompt) and returns to the plain
recording row when `autoStop` clears.

Menu bar popover (`MenuBarView.recordingSection`): under the status line, a `MessageRow(kind:
.warning)` reading "<App> closed the microphone. Stopping in 1:29." with a "Keep recording"
button on the row. Menu bar label unchanged (elapsed time). Sidebar control
(`RecordingControlPresentation`): the same line and button under the Stop control.

Device change, all surfaces: `lastWarning` set to "Audio devices changed; the recording
continues." on `.deviceResumed`, and "Audio devices changed; reconnecting…" on
`.deviceChanged` (replaced by the resumed line, or by the failure). On `.deviceLost` the
existing error and warning stay as they are.

### Copy

- Countdown: "<App> closed the microphone. Stopping in m:ss." and "Keep recording". When the
  app name is unknown: "The call app closed the microphone."
- Meeting detail, a `MessageRow(kind: .info)` under the meta line, one of: "Ended
  automatically when <App> closed the microphone.", "Ended because an audio device
  disappeared; the recording up to that point was kept.", "Ended when Steno quit." `.manual`
  and nil show nothing.
- Meeting list row meta line: append "· ended automatically" for `.callEnded`, "· device lost"
  for `.deviceLost`; nothing for the others.

## Implementation steps

Two PRs: core and capture first (steps 1 to 6, `swift test` on Linux and macOS CI), then the
app (steps 7 to 11, `apps/macos` tests). The bubble row waits for the floating indicator plan
if that has not merged; the sidebar line waits for the start-recording plan.

1. **Model and migration** (`Sources/StenoCore/Model/Meeting.swift`,
   `Sources/StenoCore/Storage/Records.swift`, `Sources/StenoCore/Storage/Migrations.swift`,
   `Sources/StenoCore/Storage/LocalRecordingIntake.swift`).
   `public enum RecordingEndReason: Codable, Sendable, Equatable, Hashable { case manual,
   callEnded(appName: String?), deviceLost, quit }` with `CaseCoding` like `MeetingState`;
   `Meeting.endReason: RecordingEndReason? = nil` in the memberwise init after `state`
   (synthesised Codable omits nil, so `Tests/Fixtures/meetings/*.json` and the export golden
   are unchanged); `MeetingRecord.endReason: String?` as a JSON column; `registerMigration("v3")`
   with `ALTER TABLE meeting ADD COLUMN endReason TEXT` appended below `v2`;
   `Tests/Fixtures/snapshots/schema/v3.sql` golden. `RecordingResult.endReason:
   RecordingEndReason` (required; the app always knows); `complete` writes it in the same
   `store.update` as the duration. Tests: `Tests/StenoCoreTests/MigrationsTests.swift` and
   `SchemaSnapshotTests.swift` (v3), `ModelCodableTests.swift` (round trip of every case, nil
   omitted), `LocalRecordingIntakeTests.swift` (`complete` stores the reason).
2. **Sink and statistics** (`Sources/StenoAudio/RealTime/LaneFrameSink.swift`,
   `Sources/StenoAudio/Capture/CaptureConfiguration.swift`). `reportDeviceLost()` becomes
   `reportDeviceChange(_ reason: DeviceChangeReason)`; the latch stays an `Atomic<Bool>` and
   gains `rearmDeviceChange()` (called by the session after a successful restart, never by a
   producer); the handler type becomes `@Sendable (DeviceChangeReason) -> Void`. Add
   `writeGap(frames: Int)` that runs `beginCallback`, `writeSilence` per lane and
   `endCallback` in chunks of `StenoAudio.frameSize`, documented as producer-slot use while no
   IOProc exists. `public enum DeviceChangeReason: Sendable, Equatable { case
   defaultOutputChanged, defaultInputChanged, outputDeviceGone, inputDeviceGone,
   sampleRateChanged, synthetic }`. `CaptureStatistics` gains `deviceChanges: Int` and
   `gapSeconds: TimeInterval` (init defaults 0 so existing call sites compile).
   `public enum CaptureNotice: Sendable, Equatable { case deviceChanged(DeviceChangeReason),
   deviceResumed(attempt: Int, gapSeconds: TimeInterval), deviceLost }`. Tests:
   `Tests/StenoAudioTests/LaneFrameSinkTests.swift` (rearm allows a second report; `writeGap`
   leaves every lane with the same count; `RealTimeAllocationTests` unchanged because the gap
   fill is not in `deliver`).
3. **Synthetic backend** (`Sources/StenoAudio/Testing/SyntheticCaptureBackend.swift`).
   `loseDeviceAfter` becomes `changeDeviceAfter: TimeInterval?` reporting `.synthetic` and
   ending the producer thread as today; new `restartsThatFail: Int = 0` makes the next N
   `start` calls throw `CaptureError.inputDeviceUnavailable`; new `streamAfterRestart:
   CaptureStream?` lets a test see different latencies after the restart; `seconds` counts per
   `start` so a restarted backend delivers again. `loseDeviceAfter` has two call sites
   outside the backend (`Tests/StenoAudioTests/CaptureSessionTests.swift`,
   `apps/macos/StenoTests/TestSupport.swift:31-41`); update both in the same PR.
4. **Session rebuild** (`Sources/StenoAudio/Capture/CaptureSession.swift`). Inject `clock: any
   Clock<Duration> = ContinuousClock()`. `Active` gains `deviceChanges`, `gapSeconds`,
   `systemPeakSoFar` (max over processing threads) and `rebuildTask: Task<Void, Never>?`.
   Replace `deviceLost()` with `deviceChanged(_ reason:)`: guard `.recording` and no rebuild
   in flight; emit `.deviceChanged`; then `rebuild(attempt: 1)`: `backend.stop()`,
   `processing.stop()`, note `systemPeak`, measure the gap from the report time (frames
   already in the rings are the old device's last audio and are kept), `sink.writeGap` for the
   gap, `echoCanceller?.reset()`, `try backend.start(...)`, build a `ProcessingThread` with the
   new `farEndDelayFrames` sharing the existing `LevelSlot` (`ProcessingThread.init` gains
   `levels: LevelSlot?` to reuse one), start it, swap into `Active`, `sink.rearmDeviceChange()`,
   emit `.deviceResumed`. On a throw: attempt < 4 sleeps the backoff on the clock and retries
   (the gap keeps growing and is filled at the next attempt); attempt 4 sets
   `endedOnDeviceLoss`, runs `finish()` and ends in `.failed(.deviceLost, recording:)` as
   today, emitting `.deviceLost`. `stop()` during a rebuild cancels `rebuildTask` and finishes.
   `finish()` folds `deviceChanges`, `gapSeconds` and the max system peak into
   `CaptureStatistics`. Add `public var notices: AsyncStream<CaptureNotice>` beside `states`
   and `levels`. Tests in `Tests/StenoAudioTests/CaptureSessionTests.swift` with `ManualClock`:
   `aDeviceChangeKeepsRecordingOnTheSameFiles` (state never leaves `.recording`, notices are
   `[.deviceChanged, .deviceResumed]`, one master whose duration is the two halves plus the
   gap, `deviceChanges == 1`, `endedOnDeviceLoss == false`, `session.stream` reflects
   `streamAfterRestart`); `aRestartThatKeepsFailingEndsInDeviceLost` (three failures then
   success recovers; four failures end `.failed(.deviceLost)` after the summed backoff on the
   manual clock); `stopDuringARebuildFinalisesOnce`; the existing
   `deviceLostStopsCleanlyWithAReadableMaster` and
   `stopAfterDeviceLossReturnsTheSameRecordingEveryTime` move to `restartsThatFail: 4`;
   `ProcessingThreadTests` cover the shared `LevelSlot`.
5. **Live backend** (`Sources/StenoAudio/Capture/LiveCaptureBackend.swift`,
   `Sources/StenoAudio/Capture/AudioDevices.swift`). `Active` remembers `outputUID`,
   `micUID`, `outputID`, `micID` and whether the input was the default. The listener closure
   becomes `noteNotification(selector:)`: schedule one `DispatchWorkItem` 500 ms out on
   `listenerQueue` (cancelling a pending one), and in it resolve `defaultSystemOutput()`,
   `defaultInput()` (or `device(uid:)` for an explicit input), read `DeviceIsAlive` on the
   remembered IDs and the aggregate's `nominalSampleRate`; identical and healthy means
   `os_log` "ignored device notification <selector>" and return; otherwise
   `sink.reportDeviceChange(reason)` with the first difference found, and `os_log` the
   selector that fired. Add the aggregate's `kAudioDevicePropertyNominalSampleRate` to the
   watched list. `AudioDevices.isAlive(_:)` reads `DeviceIsAlive`. `stop()` cancels the pending
   work item. Nothing in this step touches `IOProcRunner`. Factor the comparison into a pure
   `DeviceSnapshot.difference(from:)` in the same file with a unit test; the HAL itself is
   covered by the `[manual]` checks in Verification.
6. **CLI** (`Sources/steno`, the `record` and `dev capture-spike` commands). Print each notice
   as it arrives and the new statistics at the end. No flag changes. `Tests/stenoTests` only
   if a test pins the statistics output.
7. **Recorder end reasons and notices** (`apps/macos/Steno/Recording/RecordingController.swift`,
   `apps/macos/Steno/AppController.swift`). `stop(reason: RecordingEndReason = .manual)`; the
   `.failed` observer passes `.deviceLost`; `shutdown()` passes `.quit`; `RecordingResult`
   gets the reason. A third observer task over `session.notices` sets `lastWarning` per the
   spec and clears the stale device-lost warning on resume. Tests
   (`apps/macos/StenoTests/RecordingControllerTests.swift`): update
   `testDeviceLossStopsAndEnqueuesThePartialRecording` to `restartsThatFail: 4` and assert
   `stored.endReason == .deviceLost`; add `testADeviceChangeKeepsTheRecordingAndWarns` (state
   stays `.recording`, `lastWarning` is the resumed line, the stored meeting has
   `endReason == .manual` after a manual stop); `testStopStoresTheManualReason`;
   `AppControllerTests.testShutdownStopsTheRecordingAndTheDetector` asserts `.quit`.
8. **Auto-stop policy** (new `apps/macos/Steno/Recording/AutoStopCountdown.swift`,
   `RecordingController.swift`, `apps/macos/Steno/Detection/DetectionController.swift`,
   `AppController.swift`). `@MainActor @Observable final class AutoStopCountdown` with
   `appName: String?`, `remaining: Duration`, `fractionRemaining`, `remainingText` ("1:29"),
   `begin()` ticking once a second on the injected clock (the `DetectionPromptViewModel`
   pattern, kept separate), `onElapsed`. `RecordingController`: `private(set) var autoStop:
   AutoStopCountdown?`, `private var sawForeignMicrophone = false`, `private var callAppName:
   String?`, `static let autoStopGrace: Duration = .seconds(90)`, `func microphoneActivity(_
   event: MicrophoneActivity) async` with `enum MicrophoneActivity { case opened(appName:
   String?), released }` implementing the table above, `func keepRecording()`, and
   `start(mode:callApp:)` so the prompt passes its app name. `DetectionController.handle`:
   while `isRecording`, forward both events through `var microphoneActivity:
   ((RecordingController.MicrophoneActivity) async -> Void)?` (resolving the name with
   `appName(bundleID)`), keeping the prompt dismissal; `startRecording` passes the prompt's
   `appName`. `AppController.init` wires the closure. Tests: `RecordingControllerTests` with
   `ManualClock` and `FakeProcessAudioActivity`:
   `testMicrophoneReleaseAfterACallArmsAndStopsAfterTheGrace` (advance 90 s, recorder idle,
   `endReason == .callEnded(appName: "Zen")`), `testMicrophoneReopenedCancelsTheCountdown`,
   `testKeepRecordingCancelsUntilTheNextCall`, `testInPersonNeverArms`,
   `testNoForeignMicrophoneNeverArms`, `testStopNowUsesCallEnded`;
   `DetectionTests.testEventsWhileRecordingReachTheRecorder`;
   `AppControllerTests.testDetectionPromptStartsACallRecordingWhileTheDetectorRunsOn` asserts
   the app name reached the recorder.
9. **Surfaces** (`apps/macos/Steno/MenuBar/MenuBarView.swift`,
   `apps/macos/Steno/Recording/RecordingControl.swift` from the start-recording plan,
   `apps/macos/Steno/Panels/FloatingContent.swift` and the bubble view from the floating
   indicator plan). Render `recorder.autoStop` per the spec; `BubblePresentation.make(state:autoStop:)`
   gains the row; `RecordingControlPresentation` gains `autoStopLine` and `showsKeepRecording`.
   Tests: the presentation tests those plans add gain one case each; `MenuBarViewModelTests`
   unchanged (the countdown is the recorder's).
10. **Meeting detail and list** (`apps/macos/Steno/Main/MeetingDetailView.swift`,
    `apps/macos/Steno/Main/MeetingListViewModel.swift` and the row view). Copy per the spec
    through a pure `RecordingEndReason.sentence` helper in the app
    (`apps/macos/Steno/Recording/EndReasonText.swift`) so `MeetingDetailViewModelTests` pins
    the strings.
11. **Plans and docs**. Add the pointer line to the audio capture plan's Deferred section;
    `mobile/` untouched.

## Verification

Automated: `swift test` (Linux container and macOS CI) for steps 1 to 6; `xcodebuild test` for
`apps/macos/StenoTests` for steps 7 to 10; `RealTimeAllocationTests` still green (no change to
`deliver`); `SchemaSnapshotTests` with `v3.sql`; `FixtureManifestTests` unchanged.

Manual, on the owner's Mac with a debug build and Console filtered to `uno.schmid.steno`:

1. Start a Meet or Zoom call in Zen over AirPods; start a call recording from the prompt.
   Confirm the bubble shows elapsed time and both level bars move.
2. In the Sound menu, switch output from AirPods to the MacBook speakers, wait five seconds,
   switch back. Expect: "Audio devices changed; the recording continues." twice in the menu
   bar, the bubble never leaves the recording row, Console shows the selector that fired and
   the reason, the elapsed timer does not reset.
3. Plug in a USB microphone and pick it as input in Sound, then unplug it mid-sentence.
   Expect: a change on plug (default input moved), on unplug a reconnect line, then
   "continues" once the default input falls back to the built-in microphone; no crash, no
   `.failed`.
4. Unplug the only external output while it is the default and keep it unplugged. Expect: up
   to four attempts over about 4 s, then, if the internal speakers are found, continued
   recording. The four-failure path is covered by the unit tests with `restartsThatFail: 4`.
5. End the call in Zen (leave the meeting; keep the tab open). Expect within 3 s: the bubble
   line "Zen closed the microphone. Stopping in 1:29", the same in the menu bar and sidebar.
   Click "Keep recording"; the line goes; rejoin the call; leave again; let the countdown run
   out. Expect: recording stops, the meeting processes, the detail reads "Ended automatically
   when Zen closed the microphone." and the list row says "ended automatically".
6. Start an in-person recording, open and close FaceTime audio. Expect: no countdown.
7. Open the master of step 2 in an editor: one file, contiguous, with a short silent gap at
   each switch; the transcript timestamps of words spoken just after a switch match the
   elapsed time on the bubble to within a second.
8. Quit Steno while recording. Expect: "Ended when Steno quit." on the meeting.

## Open questions

- Which devices were in use during the 56 minute call (AirPods or another Bluetooth headset,
  built-in, USB)? The owner's answer decides whether step 5's Console line is the first field
  data point or whether a wired setup fired something this plan has not named.
- Whether an aggregate whose Bluetooth sub-device is recreated by the HAL keeps running on the
  new object (aggregates reference sub-devices by UID), in which case the "identical devices"
  branch will be common and the rebuild rare. Only a Mac can tell; the plan is correct either
  way.
- A silent-tap watchdog: the tap delivering zeros for 30 s while the tapped app reports
  `IsRunningOutput` is a known Bluetooth-adjacent failure with no notification. The rebuild
  path built here is the remedy; whether to trigger it from the processing thread's system
  peak is a separate plan.
- A Settings toggle to turn the auto-stop off. Not added; "Keep recording" is one click and a
  `Settings` `Bool` needs no migration if someone asks.
- Whether `.callEnded` should also fire from a calendar event's end when no foreign process
  ever held the microphone (a call taken on a phone in speaker mode). Out of scope here.
- Whether the grace should be shorter when the recording was started from the prompt and the
  same app that opened the microphone released it, and longer for manual starts. Field use
  first.
