# Store calls run off the async workers (P40)

P40 of `.plans/2026-10-07-stable-promotion.md`, owned by the pipeline
coordinator, closing before `0.11.0-rc.1` (gate G1). It is a stability item
from the P39 work (#278, a store locked by another process parks the meeting),
not a D3 data-loss blocker. It supersedes nothing. Line references are against
`main` at b918eb57.

## What changes and why

An audit of the audio path against stalled tokio workers and a held store
found nothing that loses audio: every stage after the capture callback runs on
its own `std::thread`. The stop's two `block_on` calls poll futures that never
yield, and a stop that stays busy leaves the meeting `recording` for the
launch's recovery. What the audit did find is a set of delays. When another
process (the Swift app during the rollback window, or any program with the
database open for writing) holds SQLite's write lock, the busy wait of up to
5 s lands on the wrong threads:

- the Tauri command workers, through `bridge_call`'s synchronous `host.call`;
- the main event-loop thread, through the tray's Record, Stop and Launch at
  login;
- the services runtime's workers, through the pipeline's ~50 synchronous store
  calls, the delivery coordinator, the handover's admit and the 2 s poll;
- the host's view-model lock, which `store_changed`, every publish and several
  commands hold across store calls, and those calls queue behind a write on
  the one shared connection.

Each of these freezes the UI for up to 5 s, or for up to 15 s on the stop's
three tries. The levels, the timer and the tray stop moving, and commands stop
answering. P40 moves every store call that can wait off those threads. Reads
get connections of their own, so a read never waits for a write. The rule
becomes:

- an async worker and the main thread never wait on SQLite;
- under the host's view-model lock the store is used through a reader only, and
  the writer is never taken there.

## Decisions

### 1. Async code reaches the store through one facade over `spawn_blocking`

| Option | For | Against |
|---|---|---|
| **Facade over `spawn_blocking`** (chosen) | Three copies already exist and work: the pipeline's `off_the_workers` (`crates/steno-pipeline/src/pipeline.rs:3360`), the handover's `on_blocking_pool` (`crates/steno-handover/src/engine/mod.rs:1098`), and the services' retry in #278. Synchronous callers keep calling the store directly. Each await keeps the order within its task. | A hop per call (tens of microseconds, against milliseconds of SQLite). Closures must own what they take (`'static`). A dropped future does not cancel its write. |
| A dedicated store thread (an actor over a channel) | One place for every call | Every read queues behind a busy write, which is the head-of-line wait we are removing. Synchronous callers would block on a channel instead of a mutex. A closure that calls the store again deadlocks the thread. More code. |
| A connection-pool crate | Readers come free | A new shared dependency, and it does not move the calls off the workers by itself. Decision 2 takes the readers and leaves out the crate. |

The facade is `Store::off_the_workers(self: &Arc<Store>, body)`. It is an
`async fn` that runs `body(&Store)` on tokio's blocking pool and resumes a
panic, as the pipeline's helper does, so P13's panic wrap still sees it.

- **Where it lives.** In `steno-core`, behind a new optional cargo feature
  `tokio`, with tokio's `rt` from `[workspace.dependencies]`. The feature keeps
  the async API opt-in: `steno-core` builds and tests without a runtime, as it
  does today, where tokio is only a dev-dependency. The crates that are async
  turn it on: `steno-pipeline`, `steno-adapters`, `steno-handover` and
  `steno-services`.
- **How much it wraps.** One facade call wraps one logical step (a step's
  reads and its write together), not each method.
- **What it replaces.** The pipeline's `off_the_workers` stays for its file
  work. The handover's `on_blocking_pool` is removed, so only one store helper
  remains. Its two call sites map a panic to the I/O error they answer today,
  with `futures_util::FutureExt::catch_unwind` over the facade's future.

### 2. One writer connection, and two reader connections beside it

Today `Store` is one `Mutex<Connection>` (`crates/steno-core/src/store/mod.rs:159`).
A write that waits out the 5 s busy timeout holds that mutex, so every read in
the process waits with it. WAL lets a reader go on while another connection
writes. The Swift app's `DatabasePool` already works this way: one writer and
a set of readers.

**WAL is on in both apps.** The Rust store refuses any other mode
(`enable_wal`, `mod.rs:482-500`, `StoreError::JournalModeNotWal`). The Swift
app's `DatabasePool` requires WAL (`Sources/StenoCore/Storage/MeetingStore.swift:62-71`).
Nothing in the schema changes, so the schema parity rule has nothing to
mirror. If WP2 found it needed a schema change, that change would follow the
rule (one SQL migration, mirrored in `Migrations.swift`, proved by the parity
test).

- **The writer** stays a `Mutex<Connection>` with everything it has today: the
  migrations at open, `write`, `write_durably` with its `FullSync` pragmas
  (per connection, so on the writer only), the commit probe and the 5 s busy
  timeout. Every write still begins `IMMEDIATE`. Only `read`, `write`,
  `write_durably` and `checkpoint_durably` take the connection lock today
  (`mod.rs:270, 286, 309, 335`), so no other method needs to move.
- **The readers.** `Store::read` and the methods built on it check out one of
  `READERS = 2` reader connections, a named constant in `store/mod.rs`. Each
  reader is opened after the migrations (`mod.rs:229-235`) and set up by the
  writer's own `set_up` (`mod.rs:426-432`: busy timeout, WAL, `synchronous`,
  `checkpoint_fullfsync`), then gets `query_only = ON`, so a write through a
  reader fails loudly. A reader keeps no transaction open between calls.
- **When both readers are busy,** a read waits on a condition variable for one
  to come back. It never falls back to the writer, so a read can never queue
  behind a busy write.
- **The checkpoint** (`checkpoint_durably`) takes the writer, then every
  reader, before its `TRUNCATE`. In-process reads in flight end first, and no
  new one starts until the checkpoint returns, so the process's own readers
  cannot run its busy timeout out and fail `checkpoint_store` at launch
  (`crates/steno-handover/src/service.rs:66-71`).
- **The in-memory store** keeps its single connection for both roles, because
  separate connections to `:memory:` do not share a database. `read` sets
  `query_only` on it for the closure and clears it afterwards, so the 47 test
  uses of the in-memory store still catch a write through `read`.
- **Nesting is a deadlock rule, and stays one.** A `Store` call inside a
  `read` or `write` closure is forbidden. With two readers, two threads that
  each nest a read inside a read wait for a free reader forever. A
  thread-local "inside a store closure" flag, set for the closure's length,
  makes every `Store` entry point `debug_assert!` that it is clear.

What a reader sees:

- **A durable write.** A reader never sees half of a `write_durably`: the
  commit is atomic, and `FullSync` changes only the writer's connection.
- **A migration in another process.** It is one transaction per step, and a
  reader keeps its snapshot until its own transaction ends.

### 3. Under the view-model lock, readers only

With decision 2, a read under the host's view-model lock cannot wait on a
write. So the reload, the publish's `build` (which reloads a newly selected
detail through `sync_detail`, `host.rs:766-787`), `follow_settings` and its
section loads (`host.rs:1160-1226`), `refresh_subtitles` and `refresh_pairing`
(the phones through `paired_devices`, `crates/steno-services/src/handover.rs:261-265`)
keep their reads where they are. There is no load and apply split and no
reload mutex. Either would add nothing once the reads cannot wait, and a load
taken before a host commit could land after it.

What must leave the lock is every **write** made under it. Each takes a host
write mutex first, reads its inputs under the view-model lock, releases it for
the write, then applies the result under the lock again. The write mutex keeps
writes in arrival order and lets a second save see the first one's result.
The writes under the lock today:

| Write | Where the host makes it |
|---|---|
| The settings save, used by every section | `settings/mod.rs:49-51` |
| The notes, tags and title | `main_window/detail.rs:485` (`update_meeting`) |
| The delete, and its `Recorder::forget_recording`, which runs `checkpoint_durably` | `main_window/list.rs:273-275`, `host.rs:1695-1705`; `crates/steno-services/src/recorder.rs:1895-1906` |
| The delete's undo, `Recorder::restore_recording`, also a checkpoint, made with the lock held | `host.rs:1580-1587`; `recorder.rs:1913-1920` |
| The speaker confirm and the person it resolves | `speakers.rs:478-492` |
| `Pipeline::apply_retention`, a durable stamp through `block_on` | `main_window/detail.rs:563`; `crates/steno-services/src/pipeline.rs:329-335` |
| `Pipeline::keep_all_recordings`, the sweep's `keep_forever` | `settings/audio.rs:209`; `crates/steno-pipeline/src/retention.rs:110-118` |
| `Pipeline::process_again`, its synchronous part before the spawn | `host.rs:1793`; `main_window/detail.rs:357` |
| `Handover::revoke`, a durable write through `block_on` | `host.rs:2322`, `settings/phones.rs:132`; `crates/steno-services/src/handover.rs:301-303` |

`rerun_summary` and `redeliver` only claim and spawn (`pipeline.rs:278-290`).
The Swift import already runs with the lock released (`host.rs:2450-2460`).

**The recorder hook never waits behind a reload.** The stop calls the hook
(`self.notify()`) at `recorder.rs:1489`, before `session.stop()` finishes the
files at 1502. Today the hook is not safe there:

- `recorder_changed` takes the view-model lock to schedule `recording`
  (`host.rs:842-853`), and a reload in flight holds that lock across its
  reads (`store_changed`, `host.rs:809-831`);
- `publish` takes the publishing mutex, then the view models
  (`host.rs:688-717`), so a publish waiting on a reload also holds up the
  next one;
- when the state or the meeting moved, the hook runs `store_changed` itself,
  on the stop's thread, before it returns.

So a stop can wait behind a reload before its files finish. Readers make that
wait short, not zero: a reload still waits for a free reader. The `recording`
topic reads nothing but the recorder's status (`host.rs:617-618`), so in WP2
the hook builds that snapshot from the status alone and hands the list work to
a thread of its own. Six rules, in the order an emit runs them:

1. **One `recording` emitter, owned by the host.** A `RecordingEmitter` in
   `crates/steno-host/src/host.rs` holds the 20 Hz throttle that
   `TopicPublisher` holds for this topic today (`RECORDING_INTERVAL`,
   `publisher.rs`), behind a leaf lock of its own. The host flush thread
   (`steno-host-flush`) flushes a held `recording` through the emitter, and
   that flush takes only the leaf, never the publishing mutex or the view
   models (`publish`, `host.rs:706-740` at PR 1's head, takes both). Invariant: no
   `recording` snapshot waits behind a reload. Proved by checks 16 and 1.
2. **A state change skips the throttle.** A status whose state, meeting or
   message differs from the last one emitted goes out at once and restarts
   the interval; only a status that moved nothing but the levels waits for
   it. Owner: the emitter, under its leaf. Proved by check 16 (`Stopping`
   within 100 ms).
3. **The page-ready flag lives in the emitter.** `page_ready` sets it under
   the leaf beside the publisher's own (`publisher.is_page_ready()`,
   `host.rs:720` at PR 1's head, which sits under the view models), so the hook reads it
   without them. Nothing goes out before it, as today. Proved by check 1 and
   by check 18, which also guards the order of `page.ready` against the first
   command.
4. **One sequence, two emitters, one order.** The recorder bumps a sequence
   number in its status under its status lock. The hook, and `build` for
   `recording` (from `page_ready` and from a publish), read the status with
   its number, take the leaf, compare the number with the last one emitted,
   and emit only a newer one, while still holding the leaf. So an older
   `recording` snapshot never goes out after a newer one, whichever path
   built it. Proved by checks 16 and 2.
5. **The reload thread.** The host starts it when it is built, beside the
   flush thread, with a coalescing flag: a request while a pass runs marks one
   more pass, and the hook only sets the flag and returns. The thread ends at
   the host's shutdown; the stop's thread and the quit's thread never join
   it. A panic inside a pass is caught (`catch_unwind`), logged, and the next
   request runs a pass. Proved by check 16 (the files finish while a reload
   is held) and check 2.
6. **The recorder never calls the hook while it holds its status lock.**
   Every `notify()` follows the guard's drop (`recorder.rs:1189-1194` and its
   callers), so the emitter can take the leaf and then the status lock. Owner:
   `CaptureRecorder`. Proved by check 14, which runs every lock holder at
   once, and check 2.

A stop's `Stopping` reaches the page and the tray before any list work, and
the files finish without waiting for a reload (check 16).

**The lock order**, from first taken to last:

1. the host write mutex;
2. the publishing mutex;
3. the view models;
4. the leaf locks, taken briefly and never while waiting on another lock:
   - the `recording` emitter's leaf (WP2, rules 1 to 4), then the recorder's
     status lock (`build` reads `recorder.status()`, `host.rs:617-618`,
     `recorder.rs:1724`), then the sink slot, in that order, as an emit takes
     them;
   - the secret store's read in `reload_sections` (the secret store must not
     call back into the host);
   - a reader's checkout.

The store's writer mutex is taken only with none of 2 to 4 held. Code holding
the view-model lock never calls `store_changed`, `recorder_changed` or a
command.

**Enforced in debug builds.** The host's lock guard sets a thread-local
"view models held" flag. `Store::write`, `write_durably` and
`checkpoint_durably` `debug_assert!` that it is clear. Every test under the
lock (the host's, the services', the shell's) then fails at the first write
left under it.

### 4. #278's park stays as it is, and the stop keeps its synchronous path

A busy `StoreError` is still the park signal (`PipelineFailure::is_busy`).
The 5 s wait now happens on the blocking pool instead of a worker, so a run
that waits no longer holds back the timers, the quit or the store retry.
`store_waits` drops `worker_threads = 2` and its comment pointing to P40, and
runs on one worker. That is the proof. The services retry already runs the
resume in `spawn_blocking` and keeps it.

The recorder's start, stop and quit save already run on their own threads
(the caller's, never a services worker). They keep calling the store
synchronously through the intake's `begin`, `complete` and `fail`
(`crates/steno-pipeline/src/intake.rs:421-571`) and the pipeline's
`enqueue_stopped_recording` (`pipeline.rs:1209-1254`). So the future that
`block_on` drives at `recorder.rs:1516` still never yields, and a stop never
needs the runtime, not even its blocking pool.

Only the intake's `admit` moves, because the handover awaits it on a worker
(`intake.rs:167-257`). Its durable file copy and its commit go into the same
facade call. The copy is blocking file I/O, and the commit must follow it.

### 5. The CLI keeps its own runtime

`steno` builds its own multi-thread runtime (`crates/steno-cli/src/main.rs:34`).
`steno process` drives the pipeline there, so its runs get the facade with no
CLI change. The synchronous commands keep calling the store directly. The
readers open in the CLI's store too; that is harmless beside the app, and
P15's lock is unchanged.

### 6. Three PRs, in order

- **PR 1 (WP1): the shell and the polls, scope items 1 to 3.** No store code.
  Its tests use gated fakes, not a held SQLite lock, so nothing in it needs
  #278. It moves the host calls off the Tauri workers and the main thread,
  and moves the polls off the services workers. On its own it ends the worst
  freezes: the whole UI on a tray Stop, and the command workers. Scope item
  3's other half, that the poll never holds the view-model lock across a wait,
  is met by PR 2's readers (decision 3), so its checks are PR 2's.
- **PR 2 (WP2): the store and the async call sites.** After #278, whose
  `WriteLockHold` and `store_waits` it uses and changes. It is mostly
  mechanical over about 90 call sites, and carries the pipeline-write cases of
  checks 1, 2 and 6.
- **PR 3 (WP3): the host's writes leave the view-model lock.** After PR 2,
  and before rc.1, through the full pipeline. It changes the lock discipline,
  so its review is about the lock order in decision 3, the debug assert and
  the deadlock test (check 14).

**The plan's own commit.** #278 merged first; PR 1 was rebased onto it and
carries this plan and the P40 row, so the table reads P39 then P40.

## Work packages

### WP1: the shell and the polls (PR 1)

- **`bridge_call`** (`apps/desktop/src-tauri/src/bridge.rs:218-295`): every
  host call in it runs in `tauri::async_runtime::spawn_blocking`, with clones
  of the host and the window. That covers `host.call`, `host.page_ready`,
  `host.publish_request`, the folder chooser's `host.call` after its dialog,
  and the `settings.general.setLaunchAtLogin` arm. The window, dialog and URL
  arms stay as they are. The call goes through a seam generic over the host
  (as `host.rs`'s `MainThread` is), so a test can gate a fake.
- **The tray** (`apps/desktop/src-tauri/src/actions.rs:99-176`):
  - `record` resolves the host window on the main thread, as today, then
    hands only the `host.call` to `spawn_blocking` and returns. An error is
    reported from that thread.
  - `set_launch_at_login`, from the tray's menu (`actions.rs:131`) and from
    Settings, runs its `autostart::set_enabled` and its host call off the main
    thread. It posts `tray::note_login_item` back to the main thread.
  - The tray shows "Stopping…" from the recorder's own `Stopping` status,
    published within the 50 ms `recording` throttle once the main thread is
    free.
- **One helper and one assert.** Every off-main host call goes through a
  shell helper, `host::call_blocking`. The assert sits in `Host::call` itself.
  `main` marks its thread with a new `steno_host::mark_ui_thread()` before the
  builder, and `Host::call` `debug_assert!`s that it does not run on the
  marked thread.
- **The exemption.** The launch runs on the main thread during setup, once
  the main window is built and before the event loop runs. Its
  `host.store_changed()` (`crates/steno-services/src/app.rs:1261`) only reads
  once PR 2's readers land. It also writes there: `recover_unfinished`'s
  resume and re-export and the retention sweep (`run_sweep`,
  `app.rs:1355-1357`), when no keyring answer or Swift import is pending. Both
  are named in the host's `# Threads` doc, and WP2 keeps them where they are
  (see WP2).
- **The polls** (`crates/steno-services/src/app.rs:1207-1252`): the events
  task and the 2 s poll run `apply_meeting_event`, `store_changed` and
  `refresh_pairing` through `tokio::task::spawn_blocking` and await them. The
  poll's interval uses `MissedTickBehavior::Delay`, so a slow tick never
  bursts.
- **The docs** are corrected to match: `crates/steno-bridge/src/dispatcher.rs:25-27`,
  `crates/steno-host/src/lib.rs:23-26`, and the host's `# Threads` section.
- **Tests:** checks 5, 7 and 12 below, with gated fakes.

**As built.** The code had more host calls on those threads than the list
above, and PR 1 moves them too:

- `window.open`'s window request, from `bridge_call` and from a deep link on
  the main thread (`windows::request`, `windows.rs:286-297`), selects the
  meeting, and so reads the store. Its publish is now queued on the shell's
  ordered lane (`host::call_in_order`) after the window opens.
- The onboarding window's close (`main.rs:580-583`) reloads onboarding from
  the store on the main thread. It is queued on the lane too.
- The event follower's retention sweep (`run_sweep`, `app.rs:1218`) writes the
  store. It runs inside the follower's blocking call.

The seam is `host::ShellHost`, implemented for `AppHandle` and for the tests'
gated fake; only the `AppHandle` side reaches the managed host. The bridge's
commands await `host::call_blocking` (`bridge::host_command`) and stay
concurrent on Tauri's blocking pool, as check 5 needs. The callers that have
no one to answer (the tray's actions, a window request, the onboarding close)
queue their call on one ordered lane instead, `host::call_in_order` over
`host::Lane`: one `std::thread` of the shell's own, fed by an `mpsc` channel,
each job under `catch_unwind` (a panic is logged at error, as the job's name
alone, and the lane goes on), never joined at exit. Tauri's pools cannot keep
click order, and the main thread used to:

- two quick clicks reach the host in the order they were made;
- a Record clicked while a Stop's save waits (the recorder reads `Stopping`,
  where a start is ignored) runs once that stop has ended, and starts a
  recording;
- the tray's Launch at login reads the system's state inside its job, so a
  second click undoes the first;
- the later of two deep links is the meeting shown. The cost: a deep link's or
  `window.open`'s request waits behind a tray Stop whose save waits.

The menus' Quit (`actions::quit_from_menu`) retitles both Record rows
"Quitting…", disabled (`tray::note_quitting`), so a Record that could no
longer start a recording is never clicked unseen. The exit then waits for the
lane's jobs queued before it, for at most `SHUTDOWN_PATIENCE`
(`actions::exit_after_queued`), so a Stop whose save waits finishes with its
own end reason, as when the main thread ran the Stop and the Quit waited
behind it. The signals, a logout and the last window's close still exit at
once.

The assert is `steno_host::assert_off_ui_thread`. It sits in the shell's
`Host::call`, `page_ready`, `publish_request` and `onboarding_window_closed`,
which take any `host::HostWindow` so a fixture-host test can call each on a
marked thread. The `Stopping…` check is
`the_tray_reads_stopping_while_the_stop_still_runs`, through the real
`WindowSink` and a stand-in main thread. The smoke reads the save from the
store's row, not the log: no log line marks a save.

### WP2: the store's readers and the facade (PR 2, after #278)

- **The store** (`crates/steno-core/src/store/mod.rs`), as decision 2 says:
  - the readers, with `READERS = 2`, the wait for a free one, and `query_only`
    (also around `read` on the in-memory store);
  - the recorder hook (`crates/steno-host/src/host.rs`, `recorder_changed`):
    the `recording` snapshot from the recorder's status under a lock of its
    own, and the list reload on the host's reload thread, as decision 3 says.
    It is in WP2 because the readers are what make a reload wait on
    something other than a write;
  - the checkpoint over the writer and every reader;
  - the nesting `debug_assert!`;
  - `Store::off_the_workers` behind the `tokio` feature;
  - the `Store` doc. It says "an async host calls them from `spawn_blocking`"
    and that the host reads on the same pool; both become the rule as built.
  - `crates/steno-core/Cargo.toml` gets the feature.
- **The call sites**, by crate:
  - `crates/steno-pipeline/src/pipeline.rs`: every store call in an async
    function, around 50, from `process_held` through `persist`, `retention`
    and `stamp_deferred_retention`, the resumes, `reprocess`, `redeliver`
    and `apply_retention`. Not `enqueue_stopped_recording`
    (`pipeline.rs:1209-1254`), which the stop reaches synchronously
    (decision 4).
  - The rest of `steno-pipeline`: `retention.rs`, `lib.rs`,
    `sample_clips.rs`, the `StoreSpeakerMemory` candidates in
    `speaker_memory.rs`, and `RecordingIntake::admit` in `intake.rs` (its
    copy and its commit in one call).
  - `crates/steno-adapters/src/runtime/coordinator.rs`: the delivery rows and
    the settings reads, at lines 105-197.
  - `crates/steno-handover/src/engine/mod.rs`: `on_blocking_pool` is removed.
    Its two call sites use the facade and map a panic as decision 1 says.
  - `crates/steno-services/src`: the async callers in `app.rs`, `recovery.rs`,
    `handover.rs` and `pipeline.rs`. The synchronous recorder path stays
    (decision 4).
- **The launch's writes stay synchronous.** `recover_unfinished`'s resume and
  re-export and the retention sweep run on the main thread during setup
  (`app.rs:1355-1357`), before the event loop, with no page to freeze yet.
  WP2 keeps them there by design, unless it finds a reason to move them
  (into the launch's existing `spawn_blocking(reconcile)`).
- **The two host calls left on workers.** `Host::updates_changed`, from the
  update schedule's `on_change` (`app.rs:1312`), and `Host::phones_changed`,
  from the gated handover's follow (`app.rs:1331`), still call the host from
  runtime workers. They only read, so WP2's readers end their wait; WP2 moves
  them to the blocking pool if its checks show a wait there.
- **`crates/steno-pipeline/tests/store_waits.rs`:** back to one worker, and the
  P40 comment goes.
- **The lint.** Workspace clippy already denies `await_holding_lock`
  (`clippy::all` with `-D warnings`), so a std guard held across the facade's
  await fails the build.
- **Tests:** checks 1, 2 and 6 (a pipeline write that waits), and checks 3, 4,
  8, 10, 11, 13, 15, 16 and 18.

### WP3: the host's writes leave the view-model lock (PR 3, before rc.1)

- **The writes.** Every row of decision 3's table moves to the host write
  mutex and the read, write, apply pattern. The files are
  `crates/steno-host/src/host.rs`, `settings/mod.rs`, `settings/audio.rs`,
  `settings/phones.rs`, `main_window/detail.rs`, `main_window/list.rs` and
  `speakers.rs`.
- **The writes and the recorder's files.** `Recorder::forget_recording` and
  `restore_recording` leave the view-model lock in the same order they run in
  today. The delete forgets the folder entry first (the file operation; when
  it fails, `checkpoint_durably` makes the rows durable instead,
  `recorder.rs:1895-1906`). Then the delete commits, and then the files go
  (`list.rs:264-300`). A delete that left the rows restores the entry (or,
  failing, checkpoints durably, `recorder.rs:1913-1920`). The publish comes
  last. A kill between any two of these steps never loses a recording and
  never brings a deleted one back (check 17).
- **The assert and the docs.** The "view models held" flag and its
  `debug_assert!` in the store's three write paths. The new lock order goes
  into the host's `# Threads` section.
- **Tests:** checks 1, 2 and 6 (a host write that waits), and checks 9, 14
  and 17.

## Acceptance

Each check that needs the lock holds another connection's write transaction
(`WriteLockHold`, from #278) for 7 s. Each one runs on a runtime with one
worker, so a wait on that worker shows as a missed deadline. "Live" means the
observed thing happens within 200 ms during the hold. Every check fails without
its WP: on `main`, or, for check 13, on #278's head with one worker. Check 17
is the exception, a guard for an order that is right today.

| # | Delay from the audit | Check | Where | WP |
|---|---|---|---|---|
| 1 | Levels to the UI wait on the host lock | `levels_keep_flowing_while_another_process_holds_the_write_lock`: during a pipeline write's wait (WP2), and during a settings save's wait (WP3), each `recorder_changed` returns and its `recording` snapshot reaches the sink live | new `crates/steno-services/src/locked_store_tests.rs` | 2, 3 |
| 2 | Recorder status, the `recording` topic, device notices and the disk watch's warnings (the same hook) | `a_device_notice_reaches_the_host_while_a_write_waits`: the notices thread's hook returns live, and the auto-stop's resume is armed on time, for the same two waits | same file | 2, 3 |
| 3 | Auto-stop countdown | `an_auto_stop_under_a_held_lock_saves_once_the_lock_goes`: the countdown's stop shows `Stopping` live, and the meeting is `queued` within 1 s of the release | `crates/steno-services/src/auto_stop/tests.rs` | 2 |
| 4 | Stop and save | `a_stop_under_a_held_lock_shows_stopping_at_once_and_saves_once_the_lock_goes` (`Stopping` within 100 ms, `queued` within 1 s of the release), and `a_stop_saves_while_every_runtime_worker_is_blocked`, which pins decision 4 | `crates/steno-services/src/recorder.rs` tests | 2 |
| 5 | UI Stop button and command workers | `a_held_host_call_leaves_the_command_workers_free`: while a gated fake's call is held, another command on the same one-worker runtime answers | `apps/desktop/src-tauri/src/bridge.rs` tests | 1 |
| 6 | The UI's commands answer | `a_command_answers_while_a_write_waits`: `meetings.select` and a Settings read answer live during a pipeline write's wait (WP2), and during another window's settings save (WP3) | `locked_store_tests.rs` | 2, 3 |
| 7 | Tray and menu Record, Stop and Launch at login on the main thread | `the_trays_record_returns_before_the_stop_ends`, `the_trays_launch_at_login_returns_before_the_host_answers` and `the_tray_reads_stopping_while_the_stop_still_runs` (gated fakes); in click order on the lane, `two_quick_tray_clicks_reach_the_host_in_click_order`, `a_record_clicked_during_a_held_stop_runs_after_it`, `a_second_launch_at_login_click_undoes_the_first`, `two_window_requests_reach_the_host_in_the_order_they_came` and `the_lane_runs_its_jobs_in_order_and_past_a_panic`; the Quit, `a_quit_waits_for_the_stop_clicked_before_it`, `a_quit_goes_ahead_once_its_patience_has_passed` and `the_record_rows_read_quitting_once_quit_was_chosen`; off the caller's thread, `a_window_request_is_published_off_the_callers_thread` and `the_onboarding_close_reaches_the_host_off_the_closing_thread`; `a_host_call_on_the_marked_ui_thread_asserts`, and one fixture-host test per shell entry point that fails without its assert (`a_command_…`, `a_page_load_…`, `a_window_request_…`, `the_onboarding_close_on_the_ui_thread_asserts`); and the measured smoke `apps/desktop/scripts/locked-store-linux.sh` (about 25 s on atlas), in `rust-ci.yml`'s Linux job as "Desktop stop under a locked store" | `actions.rs`, `windows.rs`, `main.rs`, `tray.rs` and the shell's `host.rs` tests, `crates/steno-host/src/ui_thread.rs` (the assert), the script | 1 |
| 8 | Start under the lock | `a_start_under_a_held_lock_is_refused_and_the_ui_stays_live`: after about 5 s the start is refused with "Steno could not create the meeting.", no capture opened, and checks 1 and 6 hold meanwhile | `recorder.rs` tests | 2 |
| 9 | A host write of the user's own | `a_settings_save_under_a_held_lock_leaves_the_view_models_free`: the save waits and then reports the busy error, while the list, the levels and another command stay live; the same for a delete, whose checkpoint waits | `locked_store_tests.rs` | 3 |
| 10 | Detection's settings poll | `a_read_does_not_wait_behind_a_write_that_waits_on_another_connection` (the store), and `the_detection_poll_reads_the_settings_while_a_write_waits` | `crates/steno-core/src/store/mod.rs` tests, `crates/steno-services/src/detection/tests.rs` | 2 |
| 11 | Handover admit on a worker | `an_admit_under_a_held_lock_leaves_the_worker_free`: a timer ticks live during the wait, and the admit commits once the lock goes | `crates/steno-pipeline/tests/store_waits.rs` | 2 |
| 12 | The 2 s poll and the events task | `the_poll_and_the_events_task_never_block_a_worker`: a timer task on the services runtime keeps ticking live while a reload is held at a gate (a gated fake pipeline's `export_keeps_failing`, which the detail's reload calls); `the_event_follower_takes_one_event_at_a_time` (no task per waiting event) and `a_held_poll_is_not_followed_by_a_burst` (`MissedTickBehavior::Delay`) | `crates/steno-services/src/app.rs` tests | 1 |
| 13 | Pipeline runs on the workers | every `store_waits` test passes on one worker, including the quit during a run's wait | `store_waits.rs` | 2 |
| 14 | Deadlocks in the new lock order | `host_writes_reloads_levels_flushes_and_commands_at_once_never_deadlock`: under `WriteLockHold`, a host write, a `store_changed`, a level update, a flush and a command run at once, and all finish within 5 s of the release; the "view models held" assert fires on a write planted under the lock | `locked_store_tests.rs`, `crates/steno-core/src/store/mod.rs` tests | 3 |
| 15 | The readers' own rules | `a_write_through_read_fails_on_both_stores` (file and in-memory, `query_only`); `a_store_call_inside_a_store_closure_asserts`; `the_checkpoint_waits_for_reads_in_flight_and_holds_off_new_ones` | `crates/steno-core/src/store/mod.rs` tests | 2 |
| 16 | The stop's hook behind a slow reload | `a_stop_during_a_slow_reload_shows_stopping_at_once_and_finishes_its_files`: a reload is held in flight with every reader busy (a held reader), then the recorder stops; `Stopping` reaches the sink within 100 ms, and the session's files finish while the reload is still held | `crates/steno-services/src/recorder.rs` tests | 2 |
| 17 | The delete's files and rows off the lock | `a_kill_after_the_entry_is_forgotten_keeps_the_meeting`, `a_kill_after_the_delete_commits_never_brings_the_meeting_back` and `a_kill_before_a_kept_meetings_entry_is_restored_keeps_it`: a child process deletes through the host, is killed at a gate between two steps, and the next launch lists the meeting with its recording, or does not list it and adopts no master, exactly once. A guard, not a fix: it passes on `main` and fails on a WP3 that reorders the steps | `crates/steno-services/src/kill_tests.rs` | 3 |
| 18 | `page.ready` against the page's first command | `page_ready_reaches_the_host_before_the_first_method`: on a runtime with four workers, as Tauri's is, the page sends `page.ready` and, without awaiting it, a command; across 300 runs the host sees `page.ready` first every time (0 failures). It fails today: the cycle 1 probe saw the command first in 29 of 300 runs before PR 1 and 42 of 300 at its head. Not a regression of PR 1; WP2 owns it, since rule 3 moves the page-ready flag into the emitter | `apps/desktop/src-tauri/src/bridge.rs` tests, PR 2 | 2 |

**Check 7's smoke** runs on the `xvfb-recording-linux.sh` harness, with the
stand-in tray host that `close-without-tray-linux.sh:142-189` uses. It records,
holds the lock for 8 s from `python3`'s `sqlite3`, and stops through the
tray's menu (`tray-watcher-linux.py click <bus name> <path> "Stop recording"`).
The page's Ctrl+Shift+R would go through `bridge_call` instead, and off the
Mac the tray has no shortcut. It passes when the menu's row reads "Stopping…"
within 3 s of the click, checked again once the read answered (each read starts
Python, 0.13 to 0.45 s on a busy host, so 1 s failed one run in five; the old
main-thread stop held the menu 8 s and more), the meeting is `queued` within 2
s of the release (the store's row: no log line marks a save), and
`require_saved` holds.

- **Measured.** A whole run, the headless PipeWire's start included, took
  25.9 s on atlas (a debug build; the row read "Stopping…" 0.23 s after the
  click, and the meeting was queued 0.11 s after the release), and 24 s as a
  step of the hosted Linux job on PR 1's first run. That is under 2 minutes,
  so it joins `rust-ci.yml`'s Linux hosted job, beside
  `close-without-tray-linux.sh`, as "Desktop stop under a locked store".
  It runs by hand with
  `scripts/pipewire-headless.sh apps/desktop/scripts/locked-store-linux.sh target/debug/steno-desktop`.
- **Measured again with the lane and the 3 s bound** (cycle 1's fix, atlas at a
  load of 17 to 21): five runs passed in 27.0 to 35.1 s, the row reading
  "Stopping…" 0.49 to 1.67 s after the click and the meeting queued 0.16 to
  0.38 s after the release; a sixth ended before its window showed, as GTK
  could not open the Xvfb display, a harness flake. Against `main`'s binary,
  before PR 1, the smoke fails as it should: the menu did not answer within
  3 s of the click, as the main thread was waiting on the stop's save.
- **The Quit during the held stop** (the cycle 1 probe, a Quit 0.5 s after
  "Stopping…"): with the lock held 8 s the app exited 7.2 s after the Quit,
  the meeting `queued` with the reason `manual`; held 13 s, past the Quit's
  10 s patience, it exited 11.3 s after the Quit, and the stop's last try,
  under way when the shutdown came, still saved it `manual`.

Not changed, and covered by the tests that already exist: the quit save on its
`ExitGate` thread, and the launch's recovery, which already runs on the
blocking pool (`kill_tests.rs`, and #278's quit tests).

## Risks

- **A tray Quit during a stop that waits.** Before PR 1 the Quit could not
  run until the Stop's save ended, both on the main thread, so the meeting was
  saved with the reason `manual`. Now the Quit waits for the lane's jobs
  queued before it, for at most `SHUTDOWN_PATIENCE` (10 s), and the save
  finishes the same way (`a_quit_waits_for_the_stop_clicked_before_it`). When
  another process holds the lock past that patience, the exit goes ahead and
  its shutdown tries the stop's commit no more (`stop_for_quit`,
  `crates/steno-services/src/recorder.rs`). The meeting stays `recording`,
  its audio on disk, and the next launch's recovery queues it with the end
  reason `failed`. No audio is lost; that is the designed path for a store
  that stays busy.
- **One ordered lane.** A job that waits holds up the jobs behind it. A deep
  link's or `window.open`'s request, the onboarding close and the tray's next
  action wait behind a tray Stop whose save waits (up to its three tries,
  15 s). The page's own commands do not: they stay concurrent on Tauri's
  blocking pool (check 5).
- **`page.ready` against the first command (WP2, check 18).** The page sends
  `page.ready` and does not await it (`apps/macos/web/src/bridge/hooks.ts:93`),
  so on Tauri's multi-worker runtime a command sent right after it can reach
  the host first; the race predates PR 1. Today a user sees nothing of it. The
  page sends nothing else before its first snapshot (the main window's
  `page.layout` only on a resize, `apps/macos/web/src/windows/main/main-window.tsx:32`),
  and the flag is the host's, set once for every window. A command that does
  win runs, its publish builds the snapshots but emits none
  (`crates/steno-host/src/host.rs:720`), and `page_ready` then schedules
  every topic (`host.rs:1611-1614`, `publisher.rs:76-79`), so the first
  paint has the command's effect. The one trace: that early publish stamps
  `recording`'s throttle (`publisher.rs:89-100`), so the first `recording`
  snapshot, the Record control and the levels, can come one 50 ms interval
  after the other topics, from the flush thread. WP2's rule 3 moves the flag
  into the emitter and guards the order with check 18.
- **Deadlocks with the host lock.** The order and its debug assert are in
  decision 3, and check 14 runs every lock holder at once under a held lock.
  The new waits are the host write mutex and a reader's checkout. A reader
  never waits on anything but the checkout, and the writer is never taken
  under the view models.
- **Readers run out.** Two readers serve the host, the detection poll, the
  pipeline and the handover. A long read (a search, an export) holds one,
  and other reads wait for it, never for a write. The nesting assert keeps two
  nested reads from taking both readers and waiting forever.
- **Ordering of events.** Within a pipeline task, each await keeps the write
  before the event that announces it. The host reloads under its lock as
  today, so a reload never lands after a later commit's. Replies and snapshots
  were unordered already (`dispatcher.rs`), and the page does not rely on
  their order.
- **A dropped future's write still lands.** A quit or a `select!` that drops a
  facade future does not stop the closure. Code after such a drop must not
  assume the write did not happen. This is the same as a process killed
  mid-write, which recovery already handles.
- **The blocking pool.** Tokio's default is 512 threads; idle ones end after
  10 s. Each caller waiting on the writer's mutex holds one pool thread, and
  the app has a handful of such callers (the pipeline's one run, the delivery,
  the handover's uploads), far under the limit.
- **Swift parity.** None needed: no schema, no stored value, no Swift change.
  WAL is on in both apps, and the Swift app's pool already opens readers.

## What it does not do

- It does not shorten or change the 5 s busy timeout, the stop's three tries,
  or #278's park and retry.
- It does not make the recorder's start, stop or quit async. They stay on
  their own threads by design (decision 4).
- It does not move the secret store's read out of the view-model lock. On the
  Mac that read can wait on a Keychain dialog, an existing hold that has
  nothing to do with SQLite.
- It does not make every `Store` method async. Synchronous callers on their
  own threads keep the synchronous API.
- It does not change the CLI's commands, P15's lock, or the database's
  journal mode.
