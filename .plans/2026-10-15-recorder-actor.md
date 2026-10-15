# Recorder actor: one thread decides what a recording does

Status: planned 2026-10-15, not started. **It starts after the stable release
`0.11.0`** (step 7 and gate G3 of `.plans/2026-10-07-stable-promotion.md`).
**Nothing in it lands before `0.11.0-rc.1` or while the `0.11.0` candidates
run**: D9 freezes the audio path at the first candidate, and Nicolai's decision 5
of 2026-10-15 rules out restructuring the recorder or the capture before rc.1.
PR 3 also needs a new D9 ruling, below.

This plan turns the capture and recorder architecture review of 2026-10-15 into
work. The review and the decisions live outside the repository; what this plan
needs from them is written here. It supersedes no plan and changes no other plan
in its own PR: once Nicolai makes the D9 ruling, PR 3's owner records it in D9's
entry of the stable plan.

## Goal

Make every decision about a recording (start, stop, auto-stop, the outage
warnings, the disk floor, quit) one match arm in one pure state machine, run by
one thread, tested by table tests and a scripted capture under a manual clock.
A new rule, such as a new stop cause, then becomes one arm and one table row,
settled in one or two review rounds instead of today's five to seven.

The audio does not change: what the capture records, where it writes and the
order it tears down in stay as they are.

## Why

The recorder and the capture session decide one recording's state from many
threads through many locks, and each change uncovers an interleaving nobody
wrote down.

- **#244** (stall watchdog, restarts, coreaudiod restarts) took seven review
  rounds: rounds 4 to 6 on streak, gap and backoff accounting across rebuilds,
  round 7 on the window between the notice thread and the auto-stop countdown.
  Its own soak disproved "about once a minute" only with a 120 s probe.
- **#283** fixed the order of the outage lock against `session.stream()` so a
  Windows call's countdown could not end a call while its capture recovered. It
  left a stale countdown on the host (follow-ups, below).
- **#277, #287 and #290** were test-only: they moved waits to `HANG_GUARD` and
  added `HeldOutages`, because the recorder's policy tests run the real
  producer, processing and writer threads at real-time pace. #287's
  investigation found that for a non-monotone condition "no bound helps".
  `recorder.rs`'s tests hold 22 `eventually` waits, 8 sleeps and one
  `ManualClock`.
- **#286** (open) adds a third way to refuse a
  start, `starts_refused`, an `AtomicBool` beside `quitting` and `start_holds`,
  each with its own lock rule.

The cause, on `main` at c79d00b70:

| Where | Threads that change state | Locks |
|---|---|---|
| Recorder (`crates/steno-services/src/recorder.rs`, 5 907 lines) | `steno-recording-levels`, `steno-notices`, `steno-recording-watch`, one `steno-auto-stop` per armed countdown, the host's callers, the shutdown's `stop_for_quit`, and with #286 the shell's main thread | `inner`, `changed`, `disk`, `clock`, and per recording `levels`, `fallback`, `outage`; #286 adds an atomic outside them |
| Capture session (`crates/steno-audio/src/capture/session.rs`, 2 585 lines) | `steno-watch`, `steno-rebuild`, `steno-wfail`, `steno-probe`, the caller of `stop()` | one `Core.inner` mutex and its `state_changed` condvar, held across `backend.start()` for up to 10 s; the writer thread takes it to publish levels |

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
- **The lock order is prose.** Host, then recorder `inner`, then
  `outage`, `fallback`, `levels`, `clock`; the hook runs on whichever thread
  changed something, never under `inner` or `outage`. `recorder.rs` is 26 %
  comments before its tests, `session.rs` 35 %, much of it ordering rules.

The policy itself is not the problem: `CallWatch` (`crates/steno-services/src/auto_stop.rs`)
is pure and has a table test. What is missing is a seam between the recorder and
the capture (`MakeCaptureSession` returns a concrete `CaptureSession`) and one
owner for the state.

## Target design

### The recorder (PRs 1 and 2)

**One thread, `steno-recorder`, owns a pure machine.**

```rust
struct RecorderMachine {
    phase: Phase,            // Idle, Starting, Recording, Stopping, each with its epoch
    admission: Admission,    // holds: usize, refused: Option<Refusal>
    calls: CallWatch,        // unchanged policy
    capture: CaptureView,    // recovering, still trying, fallback input, stalled device
    disk: DiskWarning,
    messages: Messages,      // error and warning the status shows
}

fn step(&mut self, now: Duration, event: Event) -> Vec<Effect>;
fn next_deadline(&self) -> Option<Duration>;
```

`step` does no I/O, takes no lock and reads no clock: `now` is an argument.
Today's locked fields (`Outage`, `Fallback`, `DiskWarning`, the levels slot)
become plain fields.

**One event channel.** The thread reads one `Receiver<Event>`:

- `Command(Start | Stop | Toggle | Quit | Keep | ClearMessages | Hold | Release | RefuseStarts | RefreshPermissions, reply)`
  from the host's callers;
- `Capture { epoch, State | Notice }` from thin forwarders over the session's
  receivers, which decide nothing;
- `Levels { epoch }`: the forwarder stores the newest levels in a slot and wakes
  the thread, so 10 Hz levels never queue up behind a busy step;
- `Disk { epoch, free }` from a reader thread;
- `Done(StartFinished | StopFinished)` from the I/O worker;
- `Deadline` from the alarm.

A stale `epoch` is dropped in one place, at the top of `step`.

**Effects run off the machine:** `OpenSession`, `CloseSession(reason)`, `Save`,
`Publish`, `Log`, `ReadDisk`, `Arm(deadline)`. Anything that can block runs in
order on one I/O worker thread, `steno-recorder-io`: all of today's
`start_inner` (session start, up to 10 s), `session.stop()` with
`intake.complete`, which may wait on a locked store (P39), and
`recover_failed_stop`. The actor thread never blocks on I/O, so it answers a
command, a notice or a status read while a start or a save waits.

**Deadlines on the injectable clock, not timer threads.** The auto-stop
countdown and the 5 s disk reading are deadlines the machine returns from
`next_deadline()`. One alarm sleeps on the injected `steno_audio::Clock`
(`Clock::sleep` with a `Cancel`) until the nearest deadline, then sends
`Deadline`; the actor re-arms it when the deadline moves. The alarm holds no
state and decides nothing: the machine compares the deadline with `now` when the
event arrives, so a stale `Deadline` does nothing. This replaces the
`steno-auto-stop` thread per armed countdown and the watch's own loop. Under
`ManualClock` the alarm is one sleeper, so `wait_for_sleepers` and `advance`
drive time exactly. The disk reading runs on its own reader thread, because
`statvfs` can block on a network volume; at most one reading is in flight.

**A lock-free `status()`.** After each step that changes what the host shows,
the actor builds an `Arc<RecorderStatus>` and stores it in an `ArcSwap`
(`arc-swap`, a new entry in the root `[workspace.dependencies]`). `status()`
loads it without a lock: it never waits on the actor, and the actor never waits
on a reader. The countdown is stored as its deadline; `status()` turns it into
seconds left with `clock.now()`, so the number does not go stale between
publishes. If `arc-swap` is refused, the fallback is a leaf mutex held only for
the `Arc` clone, with nothing under it.

**Change hooks from that one thread only.** Only `steno-recorder` calls the
host's `recorder_changed`, after storing the snapshot, so the hook and
`status()` always agree. The host already assumes nothing about the hook's
thread (P40's contract: `recorder.status()` is a leaf, the hook may run on any
thread), so it does not change.

**Admission is one value.** `quitting`, `start_holds` and `starts_refused`
become `Admission { holds, refused: Option<Refusal> }`, where `Refusal` is
`Quitting` (the menu's Quit, #286) or `ShuttingDown` (`stop_for_quit`), each
with its own message. `RefuseStarts` is a non-blocking send, so the shell's
main thread can make it, and channel order refuses every start sent after it.

**Quit is a command.** `stop_for_quit` sends `Quit` and waits for the reply,
which the machine sends once the phase is `Idle` and starts are refused.
`settle`'s wait on the status condvar goes; the shell's `SHUTDOWN_PATIENCE`
stays.

**Panics.** The actor wraps each `step` and each effect in `catch_unwind` and
maps a panic to today's `Unwinding` outcomes (`START_PANICKED` and the stop's),
so a start or stop that panics still leaves the recorder idle with an error.
If the actor thread itself dies, every later command returns an error at once
and the status says so; an open session keeps writing, and the next launch
recovers it, as after a kill today.

**What the machine answers in one arm each:**

- A second stop cause while `Stopping` joins the waiters; the first cause is
  the meeting's reason.
- A stop or a quit while `Starting` stops the recording the start produces.
- `recovering` and the countdown live in the same struct, so a release's
  countdown cannot run out between the notice and the check (S2).
- A disk reading below the floor stops with the disk's reason; above it, it
  sets or clears the warning.

**The capture seam.** A small trait in steno-services:

```rust
pub trait CaptureControl: Send + Sync {
    fn start(&self, meeting_id: Uuid) -> Result<(), CaptureError>;
    fn stop(&self) -> Result<CaptureResult, CaptureError>;
    fn stream(&self) -> Option<CaptureStream>;
    fn states(&self) -> Receiver<CaptureState>;
    fn notices(&self) -> Receiver<CaptureNotice>;
    fn levels(&self) -> Receiver<LaneLevels>;
    fn write_failed(&self) -> bool;
}
```

`CaptureSession` implements it by delegation. `ScriptedCapture`, in
`steno_services::testing`, is the fake: a test sends states, notices and levels
by hand, holds `start`, `stop` or `stream` at a gate, makes a write fail, and
reads what the recorder asked of it. Its doc lists the orderings the real
session guarantees (for example, `states()` is subscribed before `start`; a
`DeviceResumed` comes before its `Delivering`), and the fake keeps them.

### The capture session (PR 3)

The same shape one layer down, in steno-audio:

- one control thread owns `Streak`, `Watch`, the probe's schedule and the
  rebuild steps (`restart_plan` is nearly pure already), with their deadlines
  on the session's clock;
- the three finalisers (stop, device loss, write failure) become events to that
  thread, which finalises once through `finish()`; `finalised`,
  `rebuild_generation`, `recordings_started` and `before_stop_waits` go;
- `backend.start()` runs on a worker, so the session mutex is never held for
  seconds;
- the writer thread stores levels into an atomic slot instead of taking the
  session mutex (`Core::publish`);
- one `events()` channel replaces `states()`, `notices()` and `levels()`, and
  the restart state is reported once, with the device a stall blames and
  whether the warning may name the microphone; the recorder's `Outage` copy and
  `microphone_is_the_master` go.

## What must not change

These bound every PR in this plan; a diff inside them sends the PR back.

- **The real-time callback.** The backends' callbacks (Core Audio's IOProc,
  `steno-pipewire`, `steno-wasapi-*`) allocate nothing, take no lock and do not
  change.
- **The lock-free rings** between the callback and `steno-process`.
- **The relay** (`FrameRelay`) from `steno-process` to the writer.
- **The writer.** `steno-writer`'s writes, syncs, drain and error reporting. PR 3
  changes only where its level store goes, from the session mutex to an atomic.
- **The teardown order** in `CaptureSession::finish()`: the watch cancelled and
  joined; the rebuild cancelled and joined, its gap and peak kept, before the
  processing, the writer and `clear()`; `backend.stop()`; the processing thread
  stopped, which drains the rings; the writer stopped, which drains the relay;
  the files closed; the asset built even when a write or the close failed, with
  the failure beside it.
- **The start order that keeps data safe** (D3), in `start_inner`: the calendar
  lookup first; the room check, refusing below the floor; the session made; the
  audio folder recorded before the meeting row, so a kill from there on leaves a
  row whose master recovery can find; the row begun; the receivers subscribed
  before `session.start`; the threads that follow a recording spawned before
  `Recording` is visible. Its compensations stay: `forget_recording` when no row
  began, `intake.fail` and `forget_recording` when the session would not start,
  `BegunMeeting` failing the meeting and removing its folder when a start
  panics.
- **Recovery** (`crates/steno-services/src/recovery.rs`), `CallWatch`'s policy,
  `Room::of`, `missing_seconds`, every user-facing sentence, the bridge
  contract and its fixtures, and the host's `Recorder` trait
  (`crates/steno-host/src/services.rs`).
- **Privacy.** No network path and no `unsafe` code are added.

## The pull requests

Each is one PR through the usual pipeline (simplify, review and tests, then
fixes). Decision 6 applies: a PR that reaches ten review rounds stops and is
reported as a structural problem. PR 2 starts only after PR 1's fake is merged,
since the fake makes PR 2 reviewable.

### PR 1: the capture seam and the scripted fake

`test(core): the recorder's policy tests run on a scripted capture`

- **Scope.** steno-services only, about 400 lines. `CaptureControl` and its
  delegation to `CaptureSession`; `MakeCaptureSession` returns
  `Arc<dyn CaptureControl>`; `ScriptedCapture` in `testing.rs`. The recorder's
  policy tests move onto the fake under `ManualClock`: auto-stop against
  recovery (`auto_stop/tests.rs`), the outage and fallback warnings, the disk
  floor and warning, quit during a start and during a stop, the start holds.
  A few end-to-end tests stay on the synthetic capture, and `kill_tests.rs`
  stays as it is.
- **Acceptance.**
  - No production behaviour changes: `recorder.rs`'s non-test diff is the
    session's type and nothing else.
  - The moved tests read no wall time: no sleeps, and `eventually` only where a
    real thread must run, bounded by `HANG_GUARD`. `HeldOutages` and the gated
    restarts are gone from the moved tests.
  - A contract test runs one script (start, a device change, a resume, a stall,
    a stop) against the synthetic capture and the fake and gets the same
    sequence of states and notices.
  - #283's mutation (the stream read before `Outage::follow`) and #244's S2
    mutations still fail a test.
  - 20 of 20 runs pass in a 1-CPU container, and the steno-services suite's
    wall time is measured before and after.
- **Risks.** Low. The fake may promise orderings the session does not keep; the
  contract test and the fake's documented orderings are the guard.

### PR 2: the recorder actor and the pure machine

`refactor(core): one recorder thread owns the recording's state`

- **Scope.** steno-services. `recorder.rs` becomes `recorder/` with
  `machine.rs` (the pure machine, about 400 lines), `actor.rs` (the thread, the
  I/O worker, the alarm, the forwarders) and the rest; its state code shrinks
  from about 1 200 production lines to about 700. `auto_stop.rs` keeps
  `CallWatch` and drops its thread. `arc-swap` joins
  `[workspace.dependencies]`. Folds in `Admission`, the `DiskWarning`
  hysteresis and #283's stale countdown (follow-ups, below).
- **Acceptance.**
  - Table tests over `step` for: auto-stop against recovery (each S2 row,
    including the one-grace cap), the disk floor and the warning with its
    hysteresis, quit during a start and during a stop, a second stop cause
    while stopping, a stale epoch, and every admission case (hold, menu Quit,
    shutdown, and a start while not idle).
  - The existing quit, save-retry, recovery and kill tests (`kill_tests.rs`, the
    D3 kills at each step of the start) pass unchanged against the actor.
  - With the fake's `start` held at a gate, `status()` answers and a `Stop` is
    accepted while the gate is closed.
  - Every `recorder_changed` call comes from `steno-recorder` (a test hook
    asserts the thread's name).
  - No `Mutex` or `RwLock` is left in the recorder's state besides the levels
    slot; the threads `steno-recording-levels`, `steno-notices`,
    `steno-recording-watch` and `steno-auto-stop` are gone.
  - #244's manual checks 2 and 3 pass again: on Forge, `sudo killall
    coreaudiod` during a call recording; on atlas's headless PipeWire,
    `kill -STOP` on a chosen `pw-loopback` source's owner and `kill -CONT` after
    20 to 30 s, with the warning, the resume and the missing-seconds note as
    #244 describes them.
- **Risks.** Medium: it rewrites the state code of the most data-loss-sensitive
  file.
  - A who-wins order drifts (a stop's reason, a quit's reply). The table tests
    name each order, and the unchanged kill and quit tests catch a lost meeting.
  - One I/O worker serialises a start behind a stop whose save waits on a
    locked store. That is today's behaviour too (a start is refused until the
    recorder is idle); the test above pins that the status stays live.
  - The host sees the hook on a different thread. P40's contract already allows
    any thread; the host's tests run unchanged.

### PR 3: the session's control loop and one events channel

`refactor(core): one control thread decides the capture session's restarts and end`

- **Prerequisite.** The new D9 ruling below, and PR 2 merged.
- **Scope.** steno-audio, about 600 lines of `session.rs`, and the recorder's
  forwarders collapsing into one, as described under "The capture session"
  above. Folds in #290's wall-time follow-up.
- **Acceptance.**
  - `crates/steno-audio/tests/restarts.rs`, `tests/session.rs`,
    `tests/sessions.rs` and the session's unit tests pass unchanged, apart from
    receivers renamed to `events()`. Every behaviour of the stable plan's P22
    row holds as written there (backoff, the log's cadence, the gap's ends, the
    warning's rules): that row is the spec.
  - The live tests on Forge keep A10's bounds: with nothing playing, the first
    callback within 100 ms of `start` returning and 200 ms of the call to
    `start`. The headless PipeWire daemon kill test
    (`a_daemon_killed_and_started_again_resumes_the_recording`) and Windows CI's
    `live_windows` pass.
  - With a backend whose `start` is held at a gate, `stream()`, the status and a
    stop answer while the gate is closed: the mutex is not held over the start.
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
steno-services and change no part of the capture; decision 5 asks no ruling
for them.

Proposed ruling, for Nicolai to confirm, change or refuse:

> After the stable release, the capture session's control plane may change in one
> PR (this plan's PR 3), on four conditions: the data plane is untouched (the
> callbacks, the rings, the relay, the writer, `finish()`'s order); every P22
> behaviour holds as written; it is not merged before the 14-day rollback window
> of `0.11.0` has closed; and it ships first in a candidate of the next minor
> release, which passes R5's soak and D12's stability count (ten real meetings
> on the Mac, five on each listed Linux target, one with a device switch during
> a call) before a stable tag carries it. A fix during that count restarts it.

The options:

- **As proposed.** The rollback window is closed first, so a regression never
  pushes Nicolai back to the Swift app.
- **Without the candidate series.** Faster, but a restart bug would reach a
  stable install with no soak. Not recommended under D3.
- **Refused.** PR 3 is dropped; the session keeps its four control threads and
  the recorder actor sits on top of them. PR 2's gains still hold, but the
  restart state stays in two copies, the mutex stays held over
  `backend.start()`, and `microphone_is_the_master` keeps its mirror.

PR 2 changes recording behaviour code too, though not the capture. It should
ship through the same candidate series as PR 3; if both merge before the count
starts, one series covers both.

## Follow-ups from merged PRs

| Follow-up | From | Where it goes |
|---|---|---|
| A macOS MSRV job. The `msrv` job (`rust-version` 1.90, `.github/workflows/rust-ci.yml`) runs on Linux only, so Mac-only code is checked against 1.90 only by hand. | #285 | Its own `ci` PR, the first after the stable release and before PR 1, so PR 3's Mac code is checked. `cargo +1.90 check --workspace --all-targets --locked` on the macOS runner (`vars.MACOS_RUNS_ON`, `macos-15` as the fallback), same triggers as the Linux job. Acceptance: green on `main`, and red on a scratch commit that uses a newer-than-1.90 API under `cfg(target_os = "macos")`. Risk: one more job on Forge, a shared machine. |
| `DiskWarning` hysteresis. `DiskWarning::set` forgets a dismissal at the first reading with room, so a reading that hovers across the 30-minute line shows a dismissed warning again at each crossing. | #285 | PR 2, as table rows. The machine keeps the minutes left at every reading, not only under the line. Proposed: the warning shows under 30 minutes; a dismissal is forgotten only at 40 minutes or more. The 40 is for review. |
| A stale countdown on a stalled Windows call. The change hook fires only after the notice thread's stream read, which can wait up to about 10 s behind a rebuild's start, so the host can show a countdown the status already hides. Cosmetic; no data lost. | #283 | PR 2: the `DeviceChanged` step marks the capture as recovering and publishes at once, so the hook and the status agree from that step on; the stream read for the stalled device moves to the forwarder, which sends it as a second event. Acceptance: with the fake's `stream()` held at a gate, the hook's status after the `DeviceChanged` shows no countdown. PR 3 removes the read altogether: the session's event names the stalled device. |
| Three ways to refuse a start: `quitting`, `start_holds`, `starts_refused`. | #286 | PR 2: `Admission { holds, refused: Option<Refusal> }`, one message slot. #286's tests (`a_start_after_a_menu_quit_is_refused_with_a_message`, `a_stop_under_way_at_a_menu_quit_still_tries_its_commit_again`) run unchanged. |
| A wall-clock verdict outside a measurement test: `asks_that_find_the_chosen_microphone_still_silent_cost_the_recording_nothing` checks the master within 0.1 s of wall time. | #290 | PR 3, counting the frames the synthetic producer delivered instead of wall time. |

## Before rc.1, outside this plan

These land before `0.11.0-rc.1` under their own owners, and this plan assumes
them:

- **#286** merges as is (decision 4). Its `starts_refused` is folded in PR 2.
- **P40's recorder hook:** `recorder.status()` is a leaf and the hook may run on
  any thread, and no host lock is added that the hook must take in a set order.
  That is the actor's contract too, so PR 2 redoes nothing.
- **A15:** the Objective-C exception catch covers the CoreML warm-up
  (`warm_up_if_installed`), which runs during a live recording. PR 2 moves the
  warm-up onto the I/O worker and keeps the catch.
