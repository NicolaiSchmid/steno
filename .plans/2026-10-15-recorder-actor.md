# Recorder actor: one thread decides what a recording does

Status: planned 2026-10-15, not started. **It starts after the stable release
`0.11.0`** (gate G3 of `.plans/2026-10-07-stable-promotion.md`): D9 freezes the
audio path at the first candidate, and Nicolai decided on 2026-10-15 that
neither the recorder nor the capture is restructured before rc.1. PR 3 also
needs a new D9 ruling, below. D, P, A, R, S and G IDs name entries of the
stable plan.

This plan turns a review of the capture and the recorder, and Nicolai's
decisions on it (both 2026-10-15), into work; everything it needs from them is
written here. It supersedes no plan and changes no other plan. Whichever way
Nicolai rules on D9 (below), the PR that records the ruling in D9's entry also
adds this plan to the stable plan's "What follows".

## Goal

Make every decision about a recording (start, stop, auto-stop, the outage
warnings, the disk floor, quit) one match arm in one pure state machine, run by
one thread, tested by table tests and a scripted capture under a manual clock.
A new rule, such as a new stop cause, then becomes one arm and one table row
whose orderings the table tests cover, instead of an interleaving found in
review (#244 took seven rounds).

The audio does not change: what the capture records, where it writes and the
order it tears down in stay as they are.

## Why

The recorder and the capture session decide one recording's state from many
threads through many locks, and each change uncovers an interleaving nobody
wrote down. Counts are on `main` at c79d00b70.

- **#244** (stall watchdog, restarts, coreaudiod restarts) took seven review
  rounds: rounds 4 to 6 on the `Streak`, gap and backoff accounting across
  rebuilds, round 7 on the window between the notice thread and the auto-stop
  countdown. Only a 120 s probe on the manual clock showed that `session.rs`'s
  doc, "about once a minute" (P22's log cadence), did not hold.
- **#283** moved the device change's recovery mark (`Outage::follow`) ahead of
  the notice thread's `session.stream()` read, so a Windows call's countdown
  could not end a call while its capture recovered. It left a stale countdown
  on the host (follow-ups, below).
- **#277 and #287** were test-only: they moved waits to `HANG_GUARD` and added
  `HeldOutages`, because the recorder's policy tests run the real producer,
  processing and writer threads at real-time pace; #290 did the same for
  `steno-audio`'s session tests. #287's investigation found that for a
  condition that can become true and then false again "no bound helps".
  `recorder.rs`'s tests hold 26 `eventually` waits, 8 sleeps and one
  `ManualClock`.
- **#286** (open) adds a third way to refuse a start, `starts_refused`:
  `quitting` and `start_holds` are fields under `inner`, `starts_refused` an
  `AtomicBool` outside it.

The cause:

| Where | Threads that change state | Locks |
|---|---|---|
| Recorder (`crates/steno-services/src/recorder.rs`, 5 907 lines) | `steno-recording-levels`, `steno-notices`, `steno-recording-watch`, one `steno-auto-stop` per armed countdown, the host's callers, the shutdown's `stop_for_quit`, and with #286 the shell's main thread | `inner` and its `changes` condvar, `changed`, `disk`, `clock`, and per recording `levels`, `fallback`, `outage`; #286 adds an atomic and the tone tests' cancel lock outside them |
| Capture session (`crates/steno-audio/src/capture/session.rs`, 2 585 lines) | `steno-watch`, `steno-rebuild`, `steno-wfail`, `steno-probe`, the caller of `stop()` | one `Core.inner` mutex (the session mutex) and its `state_changed` condvar, held across `backend.start()` for up to 10 s; the writer thread takes it to publish levels |

- **A recording ends from many places.** In the recorder: user Stop, toggle,
  quit, the auto-stop thread and the recording watch (session failed, write
  failed, disk full). Below it: the session's own finalisers (rebuild, write
  failure, stop), whose `Failed` the watch turns into a second stop. The first
  wins through `begin_stop` taking `active`, and in the session through the
  condvar wait and `finalised`.
- **Guards exist only because of the threads:** `recordings_started`,
  `rebuild_generation`, `finalised`, the test-only `before_stop_waits` hook.
- **The restart state exists twice.** The session's `Streak` and the
  recorder's `Outage` (restarting, still trying, recovering) are one state
  machine, the second rebuilt from notices. `microphone_is_the_master` mirrors
  `CaptureBackend::stall_may_be_the_microphone`; its doc says a change to
  either must go to the other.
- **The lock order is prose.** Host, then recorder `inner`, then `outage`,
  `fallback`, `levels`, `clock`; the hook runs on whichever thread
  changed something, never under `inner` or `outage`. `recorder.rs` is 26 %
  comments before its tests, `session.rs` 35 %, much of it ordering rules.

The policy itself is not the problem: `CallWatch`
(`crates/steno-services/src/auto_stop.rs`) is pure and has a table test. What is
missing is a seam between the recorder and the capture (`MakeCaptureSession`
returns a concrete `CaptureSession`) and one owner for the state.

## What this plan assumes

These land before `0.11.0-rc.1` under their own owners:

- **#286** merges as is (Nicolai, 2026-10-15).
- **P40** (added to the stable plan by #286) changes how the host takes the
  recorder's hook. Whatever its final form, the actor does not depend on it,
  since the hook runs on its own notifier thread and `status()` never waits;
  PR 2 keeps what P40 adds to the status (#286 proposes a sequence number) and
  bumps it once per stored snapshot.
- **A15** (added to the stable plan by #263) catches CoreML's Objective-C
  exceptions at the FFI boundary (the crate's `coreml` module), which also
  covers the CoreML warm-up (`warm_up_if_installed`) that runs during a live
  recording. PR 2 keeps the warm-up off the actor: the I/O worker calls
  `warm_up_if_installed` once it has sent `StartFinished`, inside a
  `catch_unwind` that logs a panic and leaves the recording going. A stop
  queued behind it waits only for its model checks, which stat the model
  files: the load itself runs on the services runtime as today, where A15's
  catch at the FFI boundary covers it.

## Target design

### The recorder (PRs 1 and 2)

**One thread, `steno-recorder` (the actor), owns a pure machine.** Around it
run threads that change no state, each described below: the I/O worker for
blocking work, the alarm for deadlines, the forwarders over the capture's
receivers, a disk reader per recording and the hook's notifier. PR 2 names
each `steno-recorder-*`.

```rust
#[derive(Clone)]
struct RecorderMachine {
    phase: Phase,            // Idle; Starting, Recording, Stopping, each with its epoch
    admission: Admission,    // holds: usize, refused: Option<Refusal>
    calls: CallWatch,        // unchanged policy
    capture: CaptureView,    // recovering, still trying, fallback input, stalled device
    disk: DiskWarning,
    tone_tests: ToneTests,   // running system audio tests and their cancels
    messages: Messages,      // error, warning and launch note the status shows
}

fn step(&mut self, now: Duration, event: Event) -> Vec<Effect>;
fn next_deadline(&self) -> Option<Duration>;
```

`step` does no I/O, takes no lock and reads no clock: `now` is an argument.
Today's locked `Outage` and `Fallback` become the plain `CaptureView`, and
`DiskWarning` a plain field; only the levels slot stays shared, with its
forwarder. The disk watch, the clock and the hook (today's `disk`, `clock` and
`changed` locks) become construction arguments or commands (`WatchDisk`,
`SetHook`), so no lock holds them.

**One event channel.** The thread reads one `Receiver<Event>`:

- `Command(Start | Stop | Toggle | Keep | ClearMessages | Hold | Release | RefuseStarts | RefreshPermissions(denied) | Quit | MicrophoneActivity | ToneTest | ToneTestEnded | NoteAdopted(count) | WatchDisk(watch) | SetHook(hook), reply)`.
  `Start` to `RefreshPermissions` mirror the `Recorder` trait's methods that
  change a recording (`Keep` is `keep_recording`, `Hold` and `Release` are
  `hold_starts` and its `StartHold`'s drop); its folder methods,
  `remember_audio_folder` to `restore_recording`, take no recorder lock today
  and stay direct calls. The menu's Quit sends `RefuseStarts`, and
  `refresh_permissions` reads the denied permissions on its caller's thread and
  sends them. `Quit` is `stop_for_quit`'s, `MicrophoneActivity` the call
  detector's, `ToneTest` and `ToneTestEnded` the system audio test's
  (`run_tone_test`, a "tone test" below), `NoteAdopted` the launch's,
  `WatchDisk` and `SetHook` the services' wiring (today's `watch_disk_with` and
  `on_change`);
- `Capture { epoch, State | Notice | StalledDevice | Input }` from thin
  forwarders over the session's receivers. They make both stream reads (the
  stalled device at a `DeliveryStalled`, the input at a `DeviceResumed`) and
  send each as its own event, so the actor calls no `CaptureControl` method;
  they decide only whether to read the stalled device, by
  `microphone_is_the_master`, as the notice thread does. A `DeviceChanged`
  clears the stalled device, and a `StalledDevice` that follows it sets it
  again;
- `Levels { epoch }`: the forwarder stores the newest levels in a slot and
  sends a wake only once the actor has taken the last one, which it does at
  every wake, before `step` and whatever the phase, so 10 Hz levels never
  queue up behind a busy step;
- `Disk { epoch, free, write_failed }` from the recording's disk reader, its
  own thread because `statvfs` and `write_failed()` can block (a network
  volume, the session mutex during a rebuild), spawned at its first reading
  and, like today's watch, never joined. At most one reading per epoch is in
  flight, so one still blocked when its recording stops holds back none of the
  next recording's; a due reading moves the disk deadline on at once, in
  flight or not, so `next_deadline()` never returns a deadline already past;
- `Done(StartFinished | StopFinished | WorkerGone)` from the I/O worker, each
  with the epoch of the effect it answers; `StartFinished` carries the input
  the session started on (`fallback_input` after `session.start`, as today)
  and the start's room, so a low-space warning shows from `Recording` on, as
  `Active::disk_warning` does today;
- `Deadline` from the alarm;
- `Dropped` when the last `CaptureRecorder` goes (below).

A stale `epoch` is dropped in one place, at the top of `step`. Events of the
epoch that is stopping still apply: the I/O worker drops the session and joins
its forwarders before it sends `StopFinished`, so everything they sent arrives
first and the stop's notes name the stalled device and the fallback input, as
`finish_stop`'s join of the notice thread makes them today. The forwarders hold
the session weakly, as the notice thread does, since a receiver ends only when
the session is gone. The disk reader also holds it weakly: a reading that
arrives while stopping is a second stop cause or a warning the stopping status
does not show, and one that arrives after `StopFinished` is dropped, since
`Idle` has no epoch.

**Replies keep today's timing**, so the `Recorder` trait's methods return when
they do today. `Start`, and a `Toggle` that starts, reply after
`StartFinished`, or at once when refused; `Stop`, and a `Toggle` that stops,
after the `StopFinished` of the stop they began, or at once when they begin
none; `Quit` once the phase is `Idle` ("Quit is a command", below).
`RefuseStarts` and `ToneTestEnded` get no reply; every other command replies
after its own step. The actor takes each reply out of its command before
`step` and holds it until it is due, so a `step` that panics loses none (below).
Each reply is sent after its step has stored its status snapshot (below), so
a `status()` read after a reply sees that step (the host's `recording_start`
reads it to select the live row or re-read the permissions). After `Hold` the
status says what an install would stop, as `hold_starts`' doc promises;
channel order and the reply now give that, not the lock the doc names.

**Effects run off the machine:** `Publish`, `Log` and `CancelToneTests` on the
actor, `ReadDisk` on the recording's disk reader (above), and `OpenSession`,
`CloseSession(reason)` and `Save` in order on one I/O worker thread,
`steno-recorder-io`. `OpenSession` is all of today's `start_inner` (session
start, up to 10 s), `CloseSession` is `session.stop()`, and `Save` is
`intake.complete`, whose busy commit waits out the 5 s busy timeout up to
`COMMIT_ATTEMPTS` (3) times, or `recover_failed_stop`. A start while tone
tests run emits `CancelToneTests` and holds `OpenSession` until the last
`ToneTestEnded` or until `TONE_TEST_PATIENCE` passes, so the I/O worker waits
on no test. The actor thread never blocks on I/O, so it answers a command, a
notice or a status read while a start or a save waits. More threads run than
today (above), but only `steno-recorder` changes state. Dropping the last
`CaptureRecorder` sends `Dropped`: the actor cancels the alarm, ends the
notifier and the I/O worker, and exits, so
`a_dropped_recorder_lets_its_countdown_go` still passes, its
`wait_for_sleepers(1)` replaced by the status check and the alarm wait below.

**Deadlines on the injectable clock, not timer threads.** The auto-stop
countdown, the 5 s disk reading and the tone tests' patience are deadlines the
machine returns from `next_deadline()`. One alarm sleeps on the injected
`steno_audio::Clock` (`Clock::sleep` with a `Cancel`) until the nearest
deadline, then sends `Deadline`; the actor re-arms it when the deadline moves.
The alarm holds no state and decides nothing: it re-checks `clock.now()`
against its deadline each time it wakes, and the machine compares the deadline
with `now` when the event arrives, so a stale `Deadline` does nothing. This
replaces the `steno-auto-stop` thread per armed countdown and
`steno-recording-watch`'s loop. Under `ManualClock` the alarm is one sleeper,
on the nearest deadline, which while recording is usually the next disk
reading's (every 20 ms in the tests' disk watch). Since a cancelled sleeper can
still be registered, tests wait for a sleeper on that deadline (a test-only
helper beside `wait_for_sleepers`) before they `advance`, and check an armed
countdown in the status, not in the sleepers.

**A lock-free `status()`.** After each step that changes what the host shows,
the actor builds an `Arc<RecorderStatus>` and stores it in an `ArcSwap`
(`arc-swap`, new in PR 2). `status()` and `is_busy()` load it without a lock:
they never wait on the actor, and the actor never waits on a reader. The
countdown is stored as its deadline; `status()` turns it into seconds left with
`clock.now()`, so the number does not go stale between publishes. If review
refuses the dependency, the fallback is a leaf mutex held only for the `Arc`
clone, with nothing under it.

**The change hook on one notifier thread.** The hook takes the host lock, can
reload the list from the store, and takes detection's state lock, so the actor
never calls it. After storing a snapshot it wakes `steno-recorder-hook`; wakes
coalesce, and the hook reads the newest snapshot, so the hook and `status()`
agree. The notifier calls the hook inside `catch_unwind`, logs a panic and keeps
serving wakes, so one panicking hook does not silence the host. The hook
already runs on any thread today (the lock order, above), so the host does not
change; only the `Recorder` trait's docs do: its sentence on reporting from the
calling thread now says the capture recorder reports from its notifier, and
`hold_starts`' doc names no lock (above).

**Admission is one value.** `quitting`, `start_holds` and `starts_refused`
become `Admission { holds, refused: Option<Refusal> }`. `Refusal` is `MenuQuit`
(the menu's Quit, #286's `starts_refused`), which says `QUITTING_REFUSAL`, or
`ShuttingDown` (`stop_for_quit`, today's `quitting`); a hold says
`INSTALLING_UPDATE`. As in #286, a start is refused in this order: silently
when `ShuttingDown` or not idle, then with `MenuQuit`'s message, then with a
hold's. A tone test is admitted only while `Idle` and no `Refusal` is set, so
none begins once a start has set `Starting` or a Quit has refused the starts. A
start waits for those already running (above), so no tone reaches a recording;
`RefuseStarts` and `Quit` cancel them and wait for none, as #286 does.
`RefuseStarts` is a non-blocking send the shell's main thread can make, and
channel order refuses every start sent after it.

**Quit is a command.** `stop_for_quit` raises the shutdown's `Cancel`, which
the I/O worker's `retrying_while` reads, so a save under way stops retrying a
busy commit as `quitting` makes it today (a menu Quit still retries). It then
sends `Quit` and waits for the reply, which the machine sends once the phase is
`Idle` and starts are refused. `CaptureRecorder::settle`'s wait on the
`changes` condvar goes; `ExitGate`'s `SHUTDOWN_PATIENCE` stays.

**Panics.** Today a start or a stop that panics leaves the recorder `Idle` with
`START_PANICKED` or `SAVE_PANICKED` (`Unwinding`); a panic elsewhere, on the
notice or levels thread or in a call under the poison-tolerant `inner`, ends
that thread or call, and the recording goes on. The actor keeps both:

- **The I/O worker** runs each effect inside `catch_unwind` and answers a panic
  with the effect's `Done`, carrying `START_PANICKED` (once `BegunMeeting` has
  undone the start) or `SAVE_PANICKED`. A guard sends `Done(WorkerGone)` if the
  thread ends, answered as a panicked start or stop, and the actor spawns a
  fresh worker before the next effect.
- **Before `step`** the actor applies what no panic may lose, as today's
  `quitting` and `starts_refused` are plain stores: the counts of `Hold`,
  `Release` and `ToneTestEnded`, the `Refusal` of `RefuseStarts` and `Quit` and
  the tone-test cancel they raise, and a `Disk` reading's in-flight mark, which
  `step` finds applied.
- **A step that panics loses only its event:** it runs on a copy of the machine
  (a few small fields, cloned on the actor thread, never on the audio path)
  kept only if `step` returns, so the phase and its epoch stay and a start or a
  recording goes on. The actor logs the panic and replies at once, refusing a
  `ToneTest`, and while `Idle` stores `START_PANICKED` for a `Start` or a
  `Toggle`. A `Quit` still waits for a stop under way, else it leaves a start or
  a recording to the next launch's recovery. A lost `Deadline` is not sent
  again (the alarm re-arms only when the deadline moves), so the disk readings
  and the countdown end, as when today's watch or countdown thread panics.
- **A panic on what a start or a stop waits for ends it:** on any `Done`, whose
  effect has run, or on a `ToneTestEnded` or `Deadline` while a start waits for
  tone tests, the actor sets the phase outside `step`: `Idle`, or, after a
  `StartFinished` with a started session, `Stopping` on the same epoch while
  the I/O worker undoes the start as `BegunMeeting` does. Every held reply is
  sent at the next `Idle`; the error is `START_PANICKED` for a start or its
  undo, else `SAVE_PANICKED`, as a panic in `finish_stop` leaves it.

If the actor thread itself dies, every later command's send fails at once and
returns, and a guard on the thread stores a status whose error is
`SAVE_PANICKED` when a session was open, else `START_PANICKED`. The I/O
worker, its channel closed, ends and drops the session, which stops it and
closes its files (`CaptureSession`'s `Drop`); the meeting stays `recording`
and the next launch recovers it, as after a kill.

**What the machine answers in one arm each:**

- A second stop cause while `Stopping` changes nothing: the first cause is the
  meeting's reason. `Stop` or `Toggle` then replies at once, as today; `Quit`
  waits for `StopFinished`.
- `Quit` while `Starting` stops the recording the start produces; `Stop` or
  `Toggle` while `Starting` is ignored, as today.
- A microphone opened while `Starting` is remembered for the countdown.
- A capture event of the starting epoch is applied as it comes (a
  `DeviceChanged` marks the capture recovering); a `Failed` is remembered and
  stops the recording with its reason as soon as `StartFinished` arrives, as
  the watch reads the states subscribed before `session.start` today.
- `recovering` and the countdown live in the same struct, so a release's
  countdown cannot run out between the notice and the check (S2).
- A failed write stops first, as the watch checks it first today; else a
  reading below the floor stops with `STOPPED_FOR_SPACE`; above the floor, the
  reading sets or clears the warning.

**The capture seam.** A small trait in `steno-services`:

```rust
pub trait CaptureControl: Send + Sync {
    fn start(&self, meeting_id: Uuid) -> Result<(), CaptureError>;
    fn stop(&self) -> Result<CaptureResult, CaptureError>;
    fn stream(&self) -> Option<CaptureStream>;
    fn states(&self) -> Receiver<CaptureState>;
    fn notices(&self) -> Receiver<CaptureNotice>;
    fn levels(&self) -> Receiver<steno_audio::LaneLevels>;
    fn write_failed(&self) -> bool;
}
```

`CaptureSession` implements it by delegation. `ScriptedCapture`, in the crate's
test-only `testing` module (`crates/steno-services/src/testing.rs`), is the
fake: a test sends states, notices and levels by hand, holds `start`, `stop` or
`stream` at a gate, makes a write fail, and reads what the recorder asked of
it. Its doc lists the orderings the real session guarantees (`states()` begins
with the current state; `notices()` sends only what follows its subscription; a
`DeviceResumed` comes before its `Delivering`; no order holds across the three
receivers), and the fake keeps them.

### The capture session (PR 3)

The same shape one layer down, in `steno-audio`:

- one control thread owns `Streak`, `Watch`, the probe's schedule and the
  rebuild steps (`restart_plan` is nearly pure already), with their deadlines
  on the session's clock;
- the three finalisers (stop, device loss, write failure) become events to that
  thread, which finalises once through `finish()`; `finalised`,
  `rebuild_generation` and `before_stop_waits` go, and `recordings_started`
  becomes the events' epoch, checked in one place;
- `backend.start()` runs on a worker, so the session mutex is never held for
  seconds; `CaptureSession::start` still returns the backend's result, waiting
  for the worker without the mutex, as D3's compensations need; a stop during
  that start is accepted at once and tears the backend down once that `start`
  has returned (the teardown order, below);
- the writer thread stores levels into an atomic slot instead of taking the
  session mutex (`Core::publish`);
- one `events()` channel replaces `states()`, `notices()` and `levels()`, and
  the restart state is reported once, with the device a stall blames and
  whether the warning may name the microphone; `CaptureView`'s copy of the
  restart state and `microphone_is_the_master` go.

## What must not change

These bound every PR in this plan; a diff inside them sends the PR back.

- **The real-time callback.** The backends' callbacks (Core Audio's IOProc,
  `steno-pipewire`, `steno-wasapi-*`) allocate nothing, take no lock and do not
  change.
- **The lock-free rings** between the callback and `steno-process`.
- **The relay** (`FrameRelay`) from `steno-process` to the writer.
- **The writer.** `steno-writer`'s writes, syncs, drain and error reporting. PR 3
  changes only where its level store goes, from the session mutex to an atomic.
- **The teardown order** in `CaptureSession::finish()`:
  1. No watch or rebuild step runs once the teardown begins (today:
     `steno-watch` and `steno-rebuild` are cancelled and joined).
  2. The rebuild's unaccounted gap and its peak are kept, before the
     processing, the writer and `clear()` run.
  3. `backend.stop()`, which never runs beside `backend.start()`.
  4. The processing thread stops, which drains the rings.
  5. The writer stops, which drains the relay.
  6. The ring drops and the frames still in the rings are counted, then the
     rings are cleared.
  7. The files are closed.
  8. The asset is built even when a write or the close failed, with the
     failure beside it.
- **The start order that keeps data safe** (D3):
  1. Before `start_inner`, the system audio tests are cancelled and waited
     for, up to `TONE_TEST_PATIENCE`.
  2. In `start_inner`: the calendar lookup.
  3. The room check, refusing below the floor.
  4. The session is made.
  5. The audio folder is recorded before the meeting row, so a kill from there
     on leaves a row whose master recovery can find.
  6. The row is begun.
  7. The receivers are subscribed before `session.start`.
  8. Whatever reads them (today the levels and notice threads; after PR 2 the
     forwarders) runs before `Recording` is visible, so a stop that takes the
     session takes them; the recording watch (after PR 2, the first disk
     reading) comes after.

  Its compensations stay: `forget_recording` when no row began, `intake.fail`
  and `forget_recording` when the session would not start, `BegunMeeting`
  failing the meeting and removing its folder when a start panics.
- **Recovery** (`crates/steno-services/src/recovery.rs`), `CallWatch`'s policy,
  `Room::of`, `missing_seconds`, every user-facing sentence, the bridge
  contract and its fixtures, and the host's `Recorder` trait
  (`crates/steno-host/src/services.rs`) apart from its doc sentences on the
  hook's thread and on the lock `hold_starts` is taken under (above).
- **Privacy.** No network path and no `unsafe` code are added.

## The pull requests

Order: the macOS MSRV job (follow-ups, below), then PR 1, PR 2 once PR 1's fake
is merged (the fake makes PR 2 reviewable), and PR 3 once PR 2 is merged and
the D9 ruling is made.

### PR 1: the capture seam and the scripted fake

`refactor(core): the recorder drives its capture through a seam, and its policy tests run on a scripted capture`

- **Scope.** `steno-services` only, about 400 lines. `CaptureControl` and its
  delegation to `CaptureSession`; `MakeCaptureSession` returns
  `Arc<dyn CaptureControl>`; `ScriptedCapture` in `testing.rs`. The recorder's
  policy tests move onto the fake under `ManualClock`: auto-stop against
  recovery (`auto_stop/tests.rs`), the outage and fallback warnings, quit
  during a start and during a stop, the start holds. The disk floor and
  warning move onto the fake on a short interval, waiting under `HANG_GUARD`,
  until PR 2 puts the reading on the clock. A few end-to-end tests stay on the
  synthetic capture, and `kill_tests.rs` stays as it is.
- **Acceptance.**
  - No production behaviour changes: `recorder.rs`'s non-test diff is the
    session's type and nothing else.
  - The moved tests read no wall time: no sleeps, and `eventually` only where a
    real thread must run, bounded by `HANG_GUARD`. `HeldOutages` and
    `GatedRestart` (the helper that holds a rebuild's restart) are gone from
    the moved tests.
  - A contract test runs one script (start, a device change, a resume, a stall,
    a stop) against the synthetic capture, and checks that the fake's
    documented orderings hold in what the real session sent.
  - #283's mutation (the stream read before `Outage::follow`) still fails
    `a_countdown_that_runs_out_while_a_stall_s_rebuild_starts_ends_no_call` on
    Windows CI, and the mutations #244 used to prove S2's recovery rule still
    fail a test.
  - 20 of 20 runs pass in a 1-CPU container, and the `steno-services` suite's
    wall time is measured before and after.
- **Risks.** Low. The fake may promise orderings the session does not keep; the
  contract test and the fake's documented orderings are the guard.

### PR 2: the recorder actor and the pure machine

`fix(core): one recorder thread owns the recording's state`

- **Scope.** `steno-services`. `recorder.rs` becomes `recorder/` with
  `machine.rs` (the pure machine, about 400 lines), `actor.rs` (the thread, the
  I/O worker, the alarm, the forwarders, the disk readers, the hook's
  notifier) and the rest; its state code shrinks from about 1 200 production
  lines to about 700.
  `auto_stop.rs` keeps `CallWatch`; its `Countdown` loses the `Cancel` the
  countdown thread slept on. `arc-swap` joins `[workspace.dependencies]`, and
  `steno-audio`'s test-only `ManualClock` gains the deadline helper (above).
  Folds in `Admission`, the `DiskWarning` hysteresis and #283's stale
  countdown (follow-ups, below).
- **Acceptance.**
  - Table tests over `step` for: auto-stop against recovery (each case of S2's
    "As built" rule, the one-grace cap included), the disk floor and the
    warning with its hysteresis, a failed write, quit during a start and during
    a stop, a `Stop` and a `Toggle` ignored while starting, a second stop cause
    while stopping, a `Failed` and a `DeviceChanged` that arrive before
    `StartFinished`, a stale epoch, a tone test refused while starting and
    after a menu Quit, and every admission case (hold, menu Quit, shutdown,
    and a start while not idle).
  - The existing quit, save-retry, recovery and kill tests pass against the
    actor with only their observation points moved: `quit_while_busy` waits
    for the shutdown's `Cancel` in place of `inner().quitting`, #286's "the
    host hears of the refusal" waits for the hook's count under `HANG_GUARD`,
    and `held_in`'s holds become `ScriptedCapture` gates on `start` and
    `stop`. Their assertions do not change: `kill_tests.rs`'s kills, from
    before the first write to the save, #277's three quit-commit tests, #286's
    tests, and the start's compensation tests in `recorder.rs`
    (`the_folder_is_recorded_before_the_master_and_forgotten_when_the_start_fails`,
    `a_start_whose_meeting_cannot_be_written_forgets_its_folder`,
    `a_start_that_panics_after_the_meeting_began_fails_it_and_leaves_no_folder`).
    Tests that catch a passing state through the hook
    (`a_microphone_opened_while_starting_is_remembered`, auto_stop's
    `watch_stopping`, the `steno-notices` sleep in
    `a_recording_on_the_fallback_microphone_names_it_in_a_warning`,
    detection's `a_prompt_never_starts_a_second_recording`) hold the fake at a
    gate or wait for the hook under `HANG_GUARD`, since wakes coalesce and the
    hook runs after the reply.
  - The two hook-panic tests
    (`a_start_that_panics_leaves_the_recorder_idle_with_an_error`,
    `a_stop_that_panics_leaves_the_recorder_idle_with_an_error`) move their
    panic into `ScriptedCapture`'s `start` and `stop`, and assert the idle
    status and its error, not a panicking caller.
  - With the fake's `start` held at a gate, `status()` answers, and a `Stop`
    gets its reply (ignored, as today), while the gate is closed.
  - Every call of the recorder's change hook comes from `steno-recorder-hook`:
    a test hook records each call's thread name for the test to assert
    afterwards, since the notifier's `catch_unwind` would catch an assert
    inside the hook. A hook that panics is logged and called again at the next
    change.
  - No `Mutex` or `RwLock` is left in the recorder's state besides the levels
    slot and, if `arc-swap` is refused, the snapshot's leaf mutex; the threads
    `steno-recording-levels`, `steno-notices`, `steno-recording-watch` and
    `steno-auto-stop` are gone.
  - #244's manual checks 2 and 3 pass again: on Forge, `sudo killall
    coreaudiod` during a call recording; under headless PipeWire
    (`scripts/pipewire-headless.sh`), `kill -STOP` on a chosen `pw-loopback`
    source's owner and `kill -CONT` after 20 to 30 s, with the warning, the
    resume and the missing-seconds note as P22 states them.
- **Risks.** Medium: it rewrites the state code of the most data-loss-sensitive
  file.
  - A who-wins order drifts (a stop's reason, a quit's reply). The table tests
    name each order, and the kill and quit tests catch a lost meeting.
  - One I/O worker serialises a start behind a stop whose save waits on a
    locked store. That is today's behaviour too (a start is refused until the
    recorder is idle); the test above pins that the status stays live.
  - The host sees the hook on the notifier thread; it already runs on any
    thread today (above), and the host's tests run unchanged.

### PR 3: the session's control loop and one events channel

`refactor(core): one control thread decides the capture session's restarts and end`

- **Prerequisite.** The new D9 ruling below, and PR 2 merged.
- **Scope.** `steno-audio`, about 600 lines of `session.rs`, and the recorder's
  forwarders collapsing into one ("The capture session", above).
  `CaptureControl`, `ScriptedCapture` and PR 1's contract test move to
  `events()`.
- **Acceptance.**
  - `crates/steno-audio/tests/restarts.rs`, `tests/session.rs` and
    `tests/sessions.rs` pass unchanged, apart from receivers renamed to
    `events()`. The session's two unit tests go with `before_stop_waits` and
    become rows of the "finalise once" check below, run through the public API.
    `a_failed_write_with_no_thread_to_end_it_shows_through_write_failed` goes
    with `steno-wfail`, or keeps `write_failed()` if the control thread cannot
    be spawned. Every behaviour of P22 holds as written there (backoff, the
    log's cadence, the gap's ends, the warning's rules): that row is the spec.
  - The live tests on Forge keep A10's bounds: with nothing playing, the first
    callback within 100 ms of `start` returning and 200 ms of the call to
    `start`. The headless PipeWire daemon kill test
    (`a_daemon_killed_and_started_again_resumes_the_recording`) and Windows CI's
    `live_windows` pass.
  - With a backend whose `start` is held at a gate, `stream()` and the status
    answer, and a stop is accepted, while the gate is closed; the stop
    completes once the gate opens: the mutex is not held over the start.
  - A device loss, a write failure and a stop at once finalise once, with one
    reason.
  - #244's manual checks 2 and 3 again, and 20 of 20 soak runs of `session` and
    `restarts` in a 1-CPU container.
- **Risks.** Medium to high. It changes how every platform restarts a capture,
  and the Mac and Windows backends run only on Forge and in Windows CI. A gap
  accounting or backoff change is easy to make and hard to see; P22's text and
  the unchanged restart tests are the guard. It ships as D9's ruling says.

## The D9 ruling PR 3 needs

D9 reads: "no change to the audio and speech path is planned after the
cutover." PR 3 changes the capture session's control plane (which thread decides
a restart, a stall and the end of a recording), not what is recorded. It still
sits inside the audio path, so it needs Nicolai's ruling. PRs 1 and 2 stay in
`steno-services` (PR 2 adds only a test helper to `steno-audio`) and change no
part of the capture, so they need none, but PR 2 has its own release rule,
below.

Proposed ruling, for Nicolai to confirm, change or refuse:

> After the stable release, the capture session's control plane may change in one
> PR (this plan's PR 3), on four conditions: the data plane is untouched (the
> callbacks, the rings, the relay, the writer apart from where it stores levels,
> `finish()`'s order); every P22 behaviour holds as written; it is not merged
> before the 14-day rollback window of `0.11.0` has closed; and it ships first in
> a candidate of the next minor release (`0.12.0`), which passes R5's soak and
> G2's stability count (D12) as written there before a stable tag carries it. A
> fix during that count restarts it.

The options:

- **As proposed.** The rollback window is closed first, so a regression never
  pushes Nicolai back to the Swift app.
- **Without the candidate series.** Faster, but a restart bug would reach a
  stable install with no soak. Not recommended under D3.
- **Refused.** PR 3 is dropped; the session keeps its four control threads and
  the recorder actor sits on top of them. PR 2's gains still hold, but the
  restart state stays in two copies, the mutex stays held over
  `backend.start()`, and `microphone_is_the_master` keeps its mirror.

**PR 2's release rule**, whatever the ruling. PR 2 rewrites the code that
decides when a recording starts and ends, so it too merges only after the
14-day rollback window of `0.11.0` has closed and ships first in a candidate
of the next minor release (`0.12.0`) that passes R5's soak and G2's stability
count. A fix during that count restarts it. If PR 2 and PR 3 both merge before
the count starts, one series covers both.

## Follow-ups this plan takes over

| Follow-up | From | Where it goes |
|---|---|---|
| A macOS MSRV job. The `msrv` job (`rust-version` 1.90, `.github/workflows/rust-ci.yml`) runs on Linux only, so Mac-only code is checked against the MSRV only by hand. | #285 | Its own `ci` PR, the first after the stable release and before PR 1, so PR 3's Mac code is checked. The Linux job's steps (`rust-version` read from `Cargo.toml`, then `cargo +<version> check --workspace --all-targets --locked`) on the macOS runner (`vars.MACOS_RUNS_ON`, `macos-15` as the fallback), same triggers as the Linux job. Acceptance: green on `main`, and red on a scratch commit that uses an API newer than the MSRV under `cfg(target_os = "macos")`. Risk: one more job on Forge, a shared machine. |
| `DiskWarning` hysteresis. `DiskWarning::set` forgets a dismissal at the first reading with room, so a reading that hovers across the 30-minute line shows a dismissed warning again at each crossing. | #285 | PR 2, as table rows. The machine keeps the minutes left at every reading, not only under the line (a helper beside `Room::of`, which stays as it is). Proposed: the warning shows under 30 minutes; a dismissal is forgotten only at 40 minutes or more. Nicolai settles the 40 in PR 2. |
| A stale countdown on a stalled Windows call. The change hook fires only after the notice thread's stream read, which can wait up to about 10 s behind a rebuild's start, so the host can show a countdown the status already hides. Cosmetic; no data lost. | #283 | PR 2: the `DeviceChanged` step marks the capture as recovering and publishes at once, so the hook and the status agree from that step on; the stalled device's stream read moves to the forwarder (`StalledDevice`, above). Acceptance: with the fake's `stream()` held at a gate, the hook's status after the `DeviceChanged` shows no countdown. PR 3 removes the read altogether: the session's event names the stalled device. |
| Three ways to refuse a start: `quitting`, `start_holds`, `starts_refused`. | #286 | PR 2: one `Admission` value (above), one message slot. #286's tests (`a_start_after_a_menu_quit_is_refused_with_a_message`, `a_stop_under_way_at_a_menu_quit_still_tries_its_commit_again`, `a_tone_test_after_a_menu_quit_is_refused`, `a_tone_test_under_way_stops_at_a_menu_quit_and_at_the_shutdown`) keep their assertions. |
