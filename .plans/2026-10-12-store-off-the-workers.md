# Store calls run off the async workers (P40)

P40 of `.plans/2026-10-07-stable-promotion.md`, owned by the pipeline, store
and export area (`wp-pse-*`), closing before `0.11.0-rc.1` (gate G1). It is a
stability item from the P39 work (#278, a store locked by another process
parks the meeting), not a D3 data-loss blocker.

**The smaller cut.** On 2026-10-15 Nicolai chose a smaller cut for rc.1. P40
closes with three steps: PR 1 (#286) as built; the read-only reader
connections, with a reader of the host's own, and the store facade (PR 2a and
2b); and a small `recording` feed with one reload owner (PR 2c). The plan's
earlier PR 3, the host's writes leaving the view-model lock, does not ship
before stable. The host actor, the change feed and a pipeline scheduler
driven by the meeting rows come after stable, in
`.plans/2026-10-15-host-actor.md`. That plan supersedes the parts of this one
that would have moved the host's writes off the view-model lock (the host
write mutex and each write's read, write and apply, the delete's undo among
them) and the change feed with its targeted reloads. Once the host actor
lands, it supersedes decision 3's lock order too. The plan's earlier
`recording` emitter, with its leaf lock and status sequence, and its reload
thread are not superseded but replaced, within this plan, by PR 2c's feed and
reload owner.

Line references are against `main` at c79d00b70. References to code that PR 1
adds or moves are against its head, 0cd9c464, which is based on bccb79b31,
not on `main`. PR 1 adds a net 19 lines to `crates/steno-host/src/host.rs`
(32 added, 13 removed), most of them in the doc at the top, so that file's
later lines shift by about as much once it merges.

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
  calls, the delivery, the handover's admit and the 2 s poll;
- the host's view-model lock, which `store_changed`, every publish and several
  commands hold across store calls, and those calls queue behind a write on
  the one shared connection.

Each of these freezes the UI for up to 5 s, or for up to 15 s on the stop's
three tries. The levels, the timer and the tray stop moving, and commands stop
answering. A second cost has nothing to do with the lock: an idle app reloads
every meeting, the open detail and the settings every 2 s, and sends the list,
the detail and the progress again each time.

After P40:

- an async worker and the main thread never wait on SQLite;
- a read never waits for a write: the host reads on a connection of its own,
  everything else on the shared reader;
- the levels, the `recording` snapshot and a stop never wait for the
  view-model lock;
- an idle app reloads nothing from the store and sends no list, detail or
  progress.

What stays until the host actor: the host's own writes (a settings save,
notes, a speaker confirm and the rest of decision 3's list) still wait under
the view-model lock, so while another process holds the write lock, the
user's own save waits up to 5 s and the other commands wait with it. That
happens only while another process writes, and nothing is lost. The delete's
undo stays under the lock too, on purpose: one small durable write of the
folder record on a refused delete, and a checkpoint only when that write
fails.

## Decisions

### 1. Async code reaches the store through one facade over `spawn_blocking`

| Option | For | Against |
|---|---|---|
| **Facade over `spawn_blocking`** (chosen) | Three copies already exist and work: the pipeline's `off_the_workers` (`crates/steno-pipeline/src/pipeline.rs:3642`), the handover's `on_blocking_pool` (`crates/steno-handover/src/engine/mod.rs:1098`), and the services' retry from #278. Synchronous callers keep calling the store directly. Each await keeps the order within its task. | A hop per call (tens of microseconds, against milliseconds of SQLite). Closures must own what they take (`'static`). A dropped future does not cancel its write. |
| A dedicated store thread (an actor over a channel) | One place for every call | Every read queues behind a busy write, which is the head-of-line wait we are removing. Synchronous callers would block on a channel instead of a mutex. A closure that calls the store again deadlocks the thread. More code. |
| A connection-pool crate | Readers come free | A new shared dependency, and it does not move the calls off the workers by itself. Decision 2 takes the readers and leaves out the crate. |

The facade is two `async fn`s over one helper: `Store::reading(self:
&Arc<Store>, body)` for a step that only reads, and `Store::writing(self:
&Arc<Store>, body)` for a step that writes. Both run `body(&Store)` on tokio's
blocking pool today and resume a panic, as the pipeline's helper does, so
P13's panic wrap still sees it. The body is `FnOnce(&Store) -> Result<T> +
Send + 'static` and returns an owned value, so `writing`'s body can later run
on a writer thread (`.plans/2026-10-15-host-actor.md`) without touching a call
site, while `reading` stays on the pool.

- **Where it lives.** In `steno-core`, behind a new optional cargo feature
  `tokio`, with tokio's `rt` from `[workspace.dependencies]`. The feature keeps
  the async API opt-in: `steno-core` builds and tests without a runtime, as it
  does today, where tokio is only a dev-dependency. The crates that are async
  turn it on: `steno-pipeline`, `steno-adapters`, `steno-handover` and
  `steno-services`.
- **How much it wraps.** One facade call wraps one logical step (a step's
  reads and its write together, through `writing`), not each method.
- **What it replaces.** The pipeline's `off_the_workers` stays for its file
  work. The handover's `on_blocking_pool` is removed, so only one store helper
  remains. Its two call sites map a panic to the I/O error they answer today,
  with `futures_util::FutureExt::catch_unwind` over the facade's future.

### 2. One writer connection, and two reader connections: the host's own and a shared one

Today `Store` is one `Mutex<Connection>` (`crates/steno-core/src/store/mod.rs:158-163`).
A write that waits out the 5 s busy timeout holds that mutex, so every read in
the process waits with it. WAL lets a reader go on while another connection
writes. The Swift app's `DatabasePool` already works this way: one writer and
a set of readers.

**WAL is on in both apps.** The Rust store refuses any other mode
(`enable_wal`, `mod.rs:482`, `StoreError::JournalModeNotWal`). The Swift
app's `DatabasePool` requires WAL (`Sources/StenoCore/Storage/MeetingStore.swift:62-73`).
Nothing in the schema changes, so the schema parity rule has nothing to
mirror.

- **The writer** stays a `Mutex<Connection>` with everything it has today: the
  migrations at open, `write`, `write_durably` with its `FullSync` pragmas
  (per connection, so on the writer only), the commit probe and the 5 s busy
  timeout. Every write still begins `IMMEDIATE`. Only `snapshot` (under
  `read`), `write`, `write_durably` and `checkpoint_durably` take the
  connection lock today (`mod.rs:270, 286, 309, 335`), and every commit goes
  through `commit_on` (`mod.rs:364`), so no other method needs to move.
- **The readers.** `READERS = 2`, a named constant in `store/mod.rs`, is the
  most reader connections a process opens:
  - **the shared reader**, a `Mutex<Connection>` that `Store::read` and the
    methods built on it use. The detection poll, the pipeline, the handover,
    the delivery and the CLI read through it;
  - **the host's reader**, opened by `Store::with_own_reader()`, which returns
    a second `Store` over the same writer whose reads use a connection of its
    own. While a host reader is open, a second call returns an error; once
    that handle is dropped, a new host (a test that rebuilds one) can open
    another. The host
    keeps the handle inside its `Inner` (decision 3), so only the holder of
    the view-model lock reads on it. The CLI never calls it, so it opens one
    reader.

  Each reader is opened after the migrations (`Store::new`, `mod.rs:230-236`)
  and set up by the writer's own `set_up` (`mod.rs:426-432`: busy timeout,
  WAL, `synchronous`, `checkpoint_fullfsync`), then gets `query_only = ON`, so
  a write through a reader fails loudly. A reader keeps no transaction open
  between calls. Each reader is one mutex: there is no checkout among readers
  and no condition variable, and a read never falls back to the writer, so it
  can never queue behind a busy write.
- **The checkpoint** (`checkpoint_durably`) takes the writer, then the shared
  reader, then the host's, before its `TRUNCATE`. In-process reads in flight
  end first, and no new one starts until the checkpoint returns, so the
  process's own readers cannot run its busy timeout out and fail
  `checkpoint_store` at launch (`crates/steno-handover/src/service.rs:58-68`).
  When the checkpoint runs under the view-model lock (the undo's fallback,
  until the host actor), the host's reader is idle, since only that lock's
  holder reads on it, so taking it there cannot wait.
- **The in-memory store** keeps its single connection for every role, because
  separate connections to `:memory:` do not share a database;
  `with_own_reader` returns a handle over that same connection. `read` sets
  `query_only` on it for the closure and clears it afterwards, so the tests'
  in-memory stores still catch a write through `read`. Its writer and readers
  are one `std` mutex, which is not reentrant, so its checkpoint takes that
  lock once and returns, as it does today (`wal_frames < 0`, `mod.rs:348`);
  taking the writer and then a reader there would wait on itself.
  `checkpoint_store` runs it at launch in the app tests.
- **Nesting is a deadlock rule, and stays one.** A `Store` call inside a
  `read` or `write` closure is forbidden: a nested read on the same reader
  waits on its own mutex forever. A thread-local set of the databases the
  thread is inside a store call on, each entered for the call's length,
  makes every `Store` entry point `debug_assert!` that its database is not
  in it.

What a reader sees:

- **A durable write.** A reader never sees half of a `write_durably`: the
  commit is atomic, and `FullSync` changes only the writer's connection.
- **A migration in another process.** It is one transaction per step, and a
  reader keeps its snapshot until its own transaction ends.

### 3. What runs under the view-model lock, and the lock order before stable

**Reads under the lock go to the host's reader.** With decision 2, a read
under the host's view-model lock cannot wait on a write. So the reload, the
publish's `build` (which reloads a newly selected detail through
`sync_detail`, `host.rs:785-807`), `follow_settings` and its section loads
(`host.rs:1188-1254`), `refresh_subtitles` and `refresh_pairing` keep their
reads where they are: the host's own on `Inner`'s store handle, the
services' (the phones through `paired_devices`) on the shared reader. There
is no load and apply split and no reload mutex. Either would add nothing once the reads cannot
wait, and a load taken before a host commit could land after it.

**The host's writes stay under the lock until stable.** Moving them out
needs either a write mutex and a read, write, apply pattern for each, or the
host actor. The actor removes the lock instead of adding a tier to it, so the
writes wait for it (`.plans/2026-10-15-host-actor.md`). The writes under the
lock today:

| Write | Where the host makes it |
|---|---|
| The settings save, used by every section | `settings/mod.rs:51` |
| The notes, tags and title | `main_window/detail.rs:485` (`update_meeting`) |
| The delete's commit and its files | `main_window/list.rs:264-302`, under `Host::command` from `meetings_delete` (`host.rs:1804-1829`) |
| The delete's undo, `Recorder::restore_recording`: one small durable write of the folder record on a refused delete and, only when that write fails, a checkpoint | `ForgottenEntry::drop`, `host.rs:1665-1680`; `crates/steno-services/src/recorder.rs:2160-2199`. It keeps the lock on purpose, so another prompt's delete of the same meeting cannot commit between the row's read and the restore; it leaves the lock with the host actor |
| The speaker confirm and the person it resolves | `speakers.rs:478-492` |
| `Pipeline::apply_retention`, a durable stamp through `block_on` | `main_window/detail.rs:564`; `crates/steno-services/src/pipeline.rs:460-466` |
| `Pipeline::keep_all_recordings`, the sweep's `keep_all` | `settings/audio.rs:215`; `crates/steno-pipeline/src/retention.rs:110` |
| `Pipeline::process_again`, its synchronous part before the spawn | `main_window/detail.rs:357`, from `meeting_process_again` (`host.rs:1871`) |
| `Handover::revoke`, a durable write through `block_on` | `host.rs:2424`, `settings/phones.rs:187`; `crates/steno-services/src/handover.rs:763-766` |

The delete itself already runs its forget (`Recorder::forget_recording`,
`recorder.rs:2160-2175`) with the lock released: `meetings_delete` calls it
after the prompt and before `Host::command` (`host.rs:1786-1797`).

**`recording` comes from a feed that reads only the recorder.** Today the
recorder's hook cannot be trusted on the stop's thread:

- `recorder_changed` takes the view-model lock to schedule `recording`
  (`host.rs:862-883`), and a reload in flight holds that lock across its
  reads (`store_changed`, `host.rs:826-850`);
- `publish` takes the publishing mutex, then the view models
  (`host.rs:705-739`), and `build` reads the recorder's status for
  `recording` with the view models held (`host.rs:633-635`);
- when the state or the meeting moved, the hook runs `store_changed` itself,
  on the stop's thread (`finish_stop` notifies at `recorder.rs:1753`, before
  `session.stop()` finishes the files at 1766).

So a stop can wait behind a reload before its files finish, and the levels
wait for every publish. PR 2c takes `recording` out of the host's publish:

- **One feed thread.** `RecordingFeed`, in a new
  `crates/steno-host/src/recording_feed.rs`, runs one `std::thread`
  (`steno-host-recording`), started in `Host::new` in place of the flush
  thread. Only that thread touches the last status it sent and the time it
  sent it, so they need no lock, and the 20 Hz throttle
  (`RECORDING_INTERVAL`, `publisher.rs:22`) moves from `TopicPublisher` into
  it. Other threads reach the feed through two `AtomicBool`s, "page ready"
  and "resend", and an unpark.
- **Each pass** swaps "resend" to false, then reads `recorder.status()` (the
  recorder's status lock, released before anything else), and sends nothing
  until the page is ready. A status goes out at once when "resend" was set or
  when its state, meeting or message differs from the last one sent; one that
  moved only the levels waits out the rest of the 50 ms, and the feed sends
  it when that interval ends. It takes the sink from the slot and emits.
  Only a status that went out stamps the interval and becomes the last one
  sent; one held back stamps nothing.
- **The hook only pokes the feed.** `Host::recorder_changed` unparks the
  thread and returns, with no lock taken, on the levels thread
  (`forward_levels`, `recorder.rs:841-858`) and on the stop's thread alike.
- **A move asks for a reload.** When the state or the meeting moved, the feed
  asks the reload owner for a pass (below) instead of running
  `store_changed`. `Inner::recorder_seen` goes. Clearing the Settings and
  onboarding `waits_for_recording` once the recorder is idle, which
  `recorder_changed` does today (`host.rs:868-875`), moves into the pass's
  light part, so it does not depend on the forced flag.
- **Page ready.** `page_ready` (`host.rs:1683-1687`) sets the publisher's flag
  and the feed's "page ready" and "resend" flags, then pokes the feed. It
  never touches the feed's last status. So every `page.ready`, a reloaded
  page's or another window's included, gets the current `recording` once,
  even from an idle recorder whose status has not moved. The start command's schedule of
  `recording` (`host.rs:2017`) pokes the feed too.
- **A direct read.** `Host::snapshot(BridgeTopic::Recording)`, which tests
  call (`app.rs:2310` at PR 1's head), builds the snapshot from
  `recorder.status()` directly once `build`'s arm goes.
- **What goes.** With `recording` out of `TopicPublisher`, no throttled topic
  is left, so the flush thread (`steno-host-flush`, `flush_loop`,
  `host.rs:1601-1626`), `next_flush_due` (`host.rs:814`) and the publisher's
  interval code go with it. One emitter needs no sequence number and no leaf
  lock. Each topic is a full state (`publisher.rs`'s doc), so the order
  between `recording` and the publish's batch does not matter.

**One reload owner, gated by the store's change mark.** Three callers run
`store_changed` today: the 2 s poll (`crates/steno-services/src/app.rs:1251-1268`
at PR 1's head), the event follower on 5 of 7 event kinds (`follow_event`,
`app.rs:1066-1087`), and the recorder's hook on a move. None asks whether
anything changed. PR 2c gives the reloads one owner:

- **The owner** is the launch's poll task. It waits for the next 2 s tick or a
  request, whichever comes first, and runs one pass on the blocking pool, as
  PR 1 runs the poll today (`off_the_workers`, `app.rs:1093`).
- **A request** is `Host::request_reload()`: it sets a "forced" flag (an
  `AtomicBool`) and wakes the owner (a `tokio::sync::Notify`). Requests made
  while a pass runs fold into one more pass: the pass swaps the flag to false
  before it reads the mark, so a request made during the pass is kept, and
  the `Notify`'s stored permit runs the next pass. Clearing the flag at the
  end instead would lose that request. It takes no lock, so anything may
  call it. The event follower requests one after it applied the event (and
  after the sweep, for `RetentionApplied`); the feed requests one on a move.
- **A pass** takes the view-model lock and always runs the light part: the
  pairing refresh, the subtitles, the withheld key and the
  `waits_for_recording` clearing, which follow things the store does not hold
  (the OS permissions, the models, the keyring, the recorder). It then
  reads the change mark (below) and reloads the list, the progress, the
  detail and the settings only when the pass was forced or the mark moved
  since the last reload. The mark is read before the reload's reads, so a
  commit that lands during a pass is caught by the next. `Host::new` reads
  the first mark before its own loads, so the first tick reloads only when
  something committed after the host was built.
- **The change mark** is `Store::change_mark()` on the host's handle, two
  numbers:
  - a commit count that the writer's commit hook bumps. The hook sees only
    this process's commits on its writer. It covers the in-memory store, whose
    one connection's `data_version` does not move for its own commits;
  - the host reader's `PRAGMA data_version`, which moves when any other
    connection commits. It is what sees the other processes: the Swift app
    during the rollback window, and the CLI.

  The hook needs rusqlite's `hooks` feature, enabled in the root
  `[workspace.dependencies]` line (`Cargo.toml:33`), not in a crate manifest.
  In PR 2c the hook only counts; posting what changed is the change feed's
  work after stable.
- **Direct callers stay.** `store_changed` stays public and runs a forced
  pass on the caller's thread: the launch's first load (`app.rs:1277`), the
  launch's reconcile once it adopted the interrupted recordings
  (`app.rs:1421` at PR 1's head), the reread after a keyring answer, and the
  host's tests.

**The lock order**, from first taken to last:

1. the publishing mutex;
2. the view models (`Inner`);
3. the store's writer;
4. a reader: the shared one, then the host's. A reader is held only inside
   its closure, and nothing is taken under it, except by the checkpoint,
   which takes both after the writer;
5. the leaves, each taken alone and briefly: the sink slot, the recorder's
   status lock, the secret store's read in `reload_sections`.

The rules it needs:

- **The feed and the recorder's hook take no host lock** (neither 1 nor 2).
  The feed takes the recorder's status lock, releases it, then takes the sink
  slot. So no `recording` snapshot and no stop waits for a reload or a write
  (checks 1, 2, 16 and 21).
- **Under the view models, nothing runs a publish, a pass, `store_changed`,
  `recorder_changed` or a command.** A publish takes the publishing mutex,
  which comes before `Inner`. Asking for a reload only sets a flag.
- **The recorder calls its hook only with its own locks released.** The hook
  still runs `detection.recorder_changed` (`app.rs:1210-1218` at PR 1's
  head), which reads the recorder's status and so takes its status lock.
- **No `Store` call inside a `Store` closure**, debug-checked (check 15).
- **The commit hook takes no lock and calls nothing.** It runs inside the
  writer's commit and only bumps the count.
- **The rules the code already has stay:** a service called under the view
  models never calls back into the host (`crates/steno-host/src/services.rs:23-40`),
  the secret store never calls the host, the sink never runs a command from
  `emit`, the dialogs run with no lock held, and `windows::Pages` hands
  requests to the shell's lane under its lock (PR 1).

The plan's earlier version needed five more: the host write mutex first, the
emitter's leaf with the status lock and the sink slot under it, the writer
only with no host lock held, readers only under the view models, and the
sequence check under the leaf. None applies in this cut: those locks do not
exist, and the host's writes stay under the view models.

### 4. #278's park stays as it is, and the stop keeps its synchronous path

A busy `StoreError` is still the park signal (`PipelineFailure::is_busy`).
The 5 s wait now happens on the blocking pool instead of a worker, so a run
that waits no longer holds back the timers, the quit or the store retry.
`store_waits` drops `worker_threads = 2` and its comment pointing to P40
(`crates/steno-pipeline/tests/store_waits.rs:12`), and runs on one worker.
That is the proof. The services retry already runs the resume in
`spawn_blocking` and keeps it.

The recorder's start, stop and quit save already run on their own threads
(the caller's, never a services worker). They keep calling the store
synchronously through the intake's `begin`, `complete` and `fail`
(`crates/steno-pipeline/src/intake.rs:421-604`) and the pipeline's
`enqueue_stopped_recording` (`pipeline.rs:1315`). So the future that
`block_on` drives at `recorder.rs:1780` still never yields, and a stop never
needs the runtime, not even its blocking pool.

Only the intake's `admit` moves, because the handover awaits it on a worker
(`intake.rs:167`). Its durable file copy and its commit go into the same
`writing` call. The copy is blocking file I/O, and the commit must follow it.

### 5. The CLI keeps its own runtime

`steno` builds its own multi-thread runtime (`crates/steno-cli/src/main.rs:34`).
`steno process` drives the pipeline there, so its runs get the facade with no
CLI change. The synchronous commands keep calling the store directly. The
CLI's store opens the shared reader and never a host's; that is harmless
beside the app, and P15's lock is unchanged.

### 6. Three steps, in order

- **PR 1 (WP1, #286): the shell and the polls.** No store code. It moves the
  host calls off the Tauri workers and the main thread, and the polls off the
  services workers. On its own it ends the worst freezes: the whole UI on a
  tray Stop, and the command workers. It merges as built.
- **PR 2a and 2b (WP2a, WP2b): the readers and the facade.** After #278, whose
  `WriteLockHold` and `store_waits` they use. 2a is about 250 lines in
  `crates/steno-core/src/store/mod.rs` plus the host's handle; 2b is mostly
  mechanical over about 90 call sites. They may share one PR. Risk: low to
  medium, from the in-memory store's double role.
- **PR 2c (WP2c): the `recording` feed and the reload owner.** After 2a,
  since the gate reads the host's reader. About 350 lines. Risk: medium to
  low; the feed has no lock under it.

The plan's earlier PR 3 (the write mutex, every write's read, write and
apply, the delete's undo among them, the "view models held" assert and the
deadlock test) moves whole to `.plans/2026-10-15-host-actor.md`.

**The plan's own commit.** #278 merged first; PR 1 was rebased onto it and
carries the plan and the P40 row, so the table reads P39 then P40. This
revision and the stable plan's P40 row and entry for the smaller cut land with
PR 2a and 2b (#299).

## Work packages

### WP1: the shell and the polls (PR 1, #286)

Status: built in #286; it merges as built.

- **`bridge_call`** (`apps/desktop/src-tauri/src/bridge.rs:224-287`): every
  host call in it runs on Tauri's blocking pool through
  `bridge::host_command` (`bridge.rs:294-310`), with clones of the host and
  the window. That covers `host.call`, `host.page_ready`, the folder
  chooser's `host.call` after its dialog, and the
  `settings.general.setLaunchAtLogin` arm. The window, dialog and URL arms
  stay as they are.
- **The tray** (`apps/desktop/src-tauri/src/actions.rs:111-230`):
  - `record` resolves the host window on the main thread, as before, then
    hands only the host call to `record_through` and returns;
  - `set_launch_at_login`, from the tray's menu and from Settings, runs its
    `autostart::set_enabled` and its host call off the main thread
    (`launch_at_login_through`), and posts `tray::note_login_item` back to the
    main thread once the host has answered;
  - the tray shows "Stopping…" from the recorder's own `Stopping` status.
- **The polls** (`crates/steno-services/src/app.rs:1195-1268`): the event
  follower and the 2 s poll run `apply_meeting_event`, `store_changed`,
  `refresh_pairing` and the follower's `run_sweep` through `off_the_workers`
  and await them, one event at a time in the bus's order. The poll's interval
  uses `MissedTickBehavior::Delay`, so a slow tick never bursts.
- **The docs** are corrected to match: `crates/steno-bridge/src/dispatcher.rs`,
  `crates/steno-host/src/lib.rs`, and the host's `# Threads` section.

**As built.** The code had more host calls on those threads than the list
above, and PR 1 moves them too:

- `window.open`'s window request, from `bridge_call` and from a deep link on
  the main thread (`windows::request`, `windows.rs:296`), selects the meeting,
  and so reads the store. Its publish is queued on the shell's ordered lane
  (`host::call_in_order`) after the window opens.
- The onboarding window's close (`main.rs:590-595`) reloads onboarding from
  the store on the main thread. It is queued on the lane too.
- The event follower's retention sweep (`run_sweep`) writes the store. It runs
  inside the follower's blocking call.

The seam is `host::ShellHost` (`apps/desktop/src-tauri/src/host.rs:61`),
implemented for `AppHandle` and for the tests' gated fake; only the
`AppHandle` side reaches the managed host. The bridge's commands await
`host::call_blocking` (`host.rs:188`) and stay concurrent on Tauri's blocking
pool, as check 5 needs. The callers that have no one to answer (the tray's
actions, a window request, the onboarding close) queue their call on one
ordered lane instead, `host::call_in_order` over `host::Lane`
(`host.rs:215-260`): one `std::thread` of the shell's own, fed by an `mpsc`
channel, each job under `catch_unwind` (a panic is logged at error, as the
job's name alone, and the lane goes on), never joined at exit. Tauri's pools
cannot keep click order, and the main thread used to:

- two quick clicks reach the host in the order they were made;
- a Record clicked while a Stop's save waits (the recorder reads `Stopping`,
  where a start is ignored) runs once that stop has ended, and starts a
  recording;
- the tray's Launch at login reads the system's state inside its job, so a
  second click undoes the first;
- the later of two deep links is the meeting shown, a request owed to a page
  that had not mounted included: `windows::Pages` hands each request to its
  publish (`windows::publish`, which only queues it) under its lock, at the
  page's `page.ready` for an owed one, so the lane gets them in the order
  they came. The cost: a deep link's or `window.open`'s request waits behind
  a tray Stop whose save waits.

The menus' Quit (`actions::quit_from_menu`, `actions.rs:236`) first refuses
every recording start, on the main thread (`Recorder::refuse_starts`, an
`AtomicBool` outside the recorder's status lock, so main never waits for it).
A Record from any surface after it, the tray's queued behind a held Stop, the
page's, the detection prompt's, is refused with "Steno is quitting. Open it
again to record." in the status, and leaves no meeting behind. The flag is not
the shutdown's `quitting`: a Stop under way keeps retrying its save. Then both
Record rows read "Quitting…", disabled, and Launch at login is disabled
(`tray::note_quitting`). The exit then waits for the lane's jobs queued
before it, for at most `SHUTDOWN_PATIENCE` (`actions::exit_after_queued`), so
a Stop whose save waits finishes with its own end reason, as when the main
thread ran the Stop and the Quit waited behind it. The signals, a logout and
the last window's close still exit at once.

The assert is `steno_host::assert_off_ui_thread`
(`crates/steno-host/src/ui_thread.rs`); `main` marks its thread with
`steno_host::mark_ui_thread()` before the builder. The assert sits in the
shell's `Host::call`, `page_ready`, `publish_request` and
`onboarding_window_closed`, which take any `host::HostWindow` so a
fixture-host test can call each on a marked thread. The `Stopping…` check is
`the_tray_reads_stopping_while_the_stop_still_runs`, through the real
`WindowSink` and a stand-in main thread. The smoke reads the save from the
store's row, not the log: no log line marks a save.

The launch is the one exemption. It runs on the main thread during setup,
once the main window is built and before the event loop runs. Its
`host.store_changed()` (`app.rs:1277`) reads, and it also writes there:
`recover_unfinished`'s resume and re-export, unless a keyring answer or the
Swift import is pending, and the retention sweep (`app.rs:1326-1331`), unless
the Swift import is pending. Both are named in the host's `# Threads` doc.

### WP2a: the readers and the host's reader (PR 2a)

- **The store** (`crates/steno-core/src/store/mod.rs`), as decision 2 says:
  - `READERS = 2`, the shared reader and `Store::with_own_reader`, each with
    `query_only` (also around `read` on the in-memory store);
  - the checkpoint over the writer and both readers;
  - the nesting `debug_assert!`;
  - the `Store` doc (`mod.rs:142-157`). It says the host reads on the same
    thread pool and that the mutex serialises every call; both become the
    rule as built.
- **The host** (`crates/steno-host/src/host.rs`): `Inner` gains the host's
  store handle, opened in `Host::new` before the view models load. The
  reads made under the lock (about 44 uses of `self.shared.store`) go
  through it. `Shared::store` and `Host::store()` stay the shared store, for
  the code that runs without the lock.
- **Tests:** checks 3, 4, 8, 10 and 15, the pipeline-write cases of checks
  1, 2 and 6, and check 17, the delete's kill tests, as a guard: 2a changes
  the checkpoint that the forget and the undo fall back to.

**As built** (#299, with 2b):

- **Five lock entry points, not four.** `Store::on_writer` (test builds
  only) hands a test the writer's connection outside a transaction. The
  tests that tuned the writer through `read` (its busy timeout, its busy
  handler, `query_only`) would now tune a reader.
- **The nesting assert is per database.** The thread keeps a set of the
  databases it is inside a store call on, not one flag, so a call on
  another database's store inside a closure (a second process's store, in
  a test) is allowed.
- **The checkpoint under another process's lock** keeps both readers
  through its busy wait, up to 5 s, accepted for its few callers (Risks,
  and the `Store` doc). One that waits for the writer holds no reader
  (`a_checkpoint_waiting_for_the_writer_holds_no_reader`).
- **In the services' checks "live" is 1 s** (`steno_services::testing::LIVE`),
  not 200 ms: a fifth of the busy timeout every regression waits out, and
  loose enough for a machine at a load of 40 to 70 under `nice 19`, where
  200 ms flaked. The store's reader tests (checks 10 and 15) keep 200 ms.
  Checks 2 and 4 add a coarse bound from the user's side, `END_TO_END`
  (2 s): check 4 from the stop to `Stopping` in the host,
  check 2 from the device change's first notice to the countdown armed
  again. Each fails with the stop sleeping 3 s before its notice, or the
  re-arm waiting on a store write. Check 4's narrow bound runs from the
  recorder's notice. Check 2's device change is held until the countdown
  is armed and the write waits (`hold_outages`), and check 1 counts at
  least three hook calls during the hold.
- **Extra tests:** `the_hosts_reads_do_not_wait_for_a_read_on_the_shared_reader`,
  `one_host_reader_at_a_time`, and `the_host_reads_on_a_reader_of_its_own`
  (`crates/steno-host/tests/main_window.rs`), which pins the host on its
  own reader.
- **The vault save checks the vault with the lock released.** The Export
  section's and onboarding page 2's save ran the destination's check, which
  creates and removes a probe file in the vault, under the view-model lock,
  so a slow folder or the Mac's Files and Folders prompt held the levels
  and the stop. The save now begins under the lock, checks outside it and
  stores under it, as page 1's vault check (#295) already ran on a thread
  of its own; only the latest of two overlapping saves stores, and an edit
  undone while a check runs begins a save of the stored value again
  (`a_held_vault_check_leaves_the_host_live`, its page 2 twin,
  `only_the_latest_of_two_vault_saves_stores` and the undo tests in
  `crates/steno-host/tests/`).

### WP2b: the facade and the async call sites (PR 2b, or with 2a)

- **The facade.** `Store::reading` and `Store::writing` behind the `tokio`
  feature; `crates/steno-core/Cargo.toml` gets the feature.
- **The call sites**, by crate:
  - `crates/steno-pipeline/src/pipeline.rs`: every store call in an async
    function, around 50, from `process_held` through `persist`, `retention`
    and `stamp_deferred_retention`, the resumes, `reprocess`, `redeliver`
    and `apply_retention`. Not `enqueue_stopped_recording`
    (`pipeline.rs:1315`), which the stop reaches synchronously (decision 4).
  - The rest of `steno-pipeline`: `retention.rs`, `lib.rs`,
    `sample_clips.rs`, the `StoreSpeakerMemory` candidates in
    `speaker_memory.rs`, and `RecordingIntake::admit` in `intake.rs` (its
    copy and its commit in one `writing` call).
  - `crates/steno-adapters/src/runtime/coordinator.rs`: the delivery rows and
    the settings reads (from line 105).
  - `crates/steno-handover/src/engine/mod.rs`: `on_blocking_pool` is removed.
    Its two call sites use the facade and map a panic as decision 1 says.
  - `crates/steno-services/src`: the async callers in `app.rs`, `recovery.rs`,
    `handover.rs` and `pipeline.rs`. The synchronous recorder path stays
    (decision 4).
- **The launch's writes stay synchronous.** `recover_unfinished`'s resume and
  re-export and the retention sweep run on the main thread during setup, with
  no page to freeze yet.
- **The two host calls left on workers.** `Host::updates_changed`, from the
  update schedule's `on_change` (`app.rs:1285` at PR 1's head), and
  `Host::phones_changed`, from the gated handover's follow (`app.rs:1304`),
  still call the host from runtime workers. Both wait for `Inner`, and a
  settings save holds `Inner` through its 5 s busy wait, so either can block
  a worker. 2b moves both to the blocking pool.
- **`store_waits.rs`:** back to one worker, and the P40 comment goes.
- **The lint.** Workspace clippy already denies `await_holding_lock`
  (`clippy::all` with `-D warnings`), so a std guard held across the facade's
  await fails the build.
- **Tests:** checks 11 and 13.

**As built** (#299, with 2a):

- **About 55 store calls became 36 facade steps:** 27 in `pipeline.rs`, 1
  in `RecordingIntake::admit` (the reads, the durable copy, the commit and
  the `failed` save), 1 in the speaker memory, 5 in the delivery coordinator and 2 in the
  handover. `retention.rs`, `lib.rs`, `sample_clips.rs`, `recovery.rs` and
  the services' `handover.rs` had no async store call: recovery's async
  functions run under `block_on` on the launch's blocking task, and the
  services' handover goes through the engine. `mark_pending` stays a
  synchronous trait method, called inside a `writing` step.
- **The phone admission runs to its end.** On `main` the admission ran from
  its reads to its enqueue in one poll. Through the facade it awaits, and
  hyper drops a request's future when the phone hangs up, which its 10 s
  timeout does under a long busy wait. So `RecordingIntake::admit` runs the
  admission (the store step, its commit included, then the enqueue) in a
  task of its own that the request awaits but cannot cancel, and the
  handover's `Engine::admit` runs in one too, which holds the `completing`
  mark and ends the receipt in memory `complete` or `failed`. A phone that
  hangs up mid-admit leaves its meeting committed and enqueued once, and its
  retry is answered from that receipt with no upload again
  (`tests/admission_under_a_held_lock.rs` in `steno-handover`); a retry
  while the admission still waits gets the 409 that lists every chunk. An
  admission in flight when the app stops (its phone hung up, or the
  listener closed its connection by force) is not waited for: it still
  finishes after the stop, or the exit cuts it as a crash would: a copy
  without its commit is noted, and the next launch adopts it (the
  follow-up's duplicate, never a loss).
- **The `failed` receipt save is tried again** while the database is busy,
  up to `RecordingIntake::FAILED_SAVE_ATTEMPTS` (3) in all, as the local
  intake's commit is, there without a back-off: the copy goes only once
  that save is on the disk.
  Before each try the intake waits `FAILED_SAVE_BACK_OFF` (200 ms) with the
  writer free, so a write of this process queued behind the commit or the
  try before runs in between and waits at most one busy timeout behind
  the admission, not three.
- **The resumes are synchronous** and call the store directly.
- **The two notifications log a panic.** `updates_changed` and
  `phones_changed` run on the blocking pool with nothing awaiting them, so
  a panic in either is logged at error, by name, without its message.
- **Check 11** holds the lock 2 s, not 7 s, and allows a 600 ms stall of the
  timer, check 12's window: a 7 s hold runs out the admit's busy timeout,
  and the admission has no retry of its commit. The check still fails on
  `main`, where the worker stands still for the whole hold. The admission
  under 7 s and 12 s holds is pinned over TLS through the real service
  instead.
- **The vault saves split into begin, validate and finish:** the begin and
  the finish run under the host's lock, the destination's check between them
  without it (WP2a's as-built).
- **Check 12** (`the_poll_and_the_events_task_never_block_a_worker`) asks
  the runtime's one worker to run a task while a reload is held at the
  gate, bounded only by `HANG_GUARD`, in place of a timer's 200 ms gaps,
  which load could fail.

### WP2c: the `recording` feed and the reload owner (PR 2c)

- **The feed** (new `crates/steno-host/src/recording_feed.rs`), as decision 3
  says. In `crates/steno-host/src/host.rs`: `recorder_changed` pokes it,
  `build`'s `Recording` arm and `Inner::recorder_seen` go, `page_ready` sets
  its "page ready" and "resend" flags, the start command pokes it, and `Host::new` starts it in place of
  the flush thread, which goes with `flush_loop` and `next_flush_due`. In
  `crates/steno-host/src/publisher.rs`: `recording` and the interval code
  leave `TopicPublisher`; its coalescing and page-ready flag stay.
- **The reload owner.** `Host::request_reload` and the pass, with the light
  part and the gate, in `host.rs`; `Inner` keeps the last mark it reloaded
  at. `Store::change_mark` and the writer's commit hook in
  `crates/steno-core/src/store/mod.rs`, with rusqlite's `hooks` feature on
  the root `[workspace.dependencies]` line. In `crates/steno-services/src/app.rs`: the
  poll task waits for a tick or a request and runs the pass, and
  `follow_event` requests a reload where it ran `store_changed`.
- **The page's first command.** The page sends `page.ready` without awaiting
  it (`usePageReady`, `apps/macos/web/src/bridge/hooks.ts:93`), so on
  Tauri's multi-worker runtime a command sent right after it can reach the
  host first. 2c does not change that order, and the page needs no change.
  What matters is the outcome, and the feed keeps it: the command's early
  publish stamps only the topics left in `TopicPublisher` (`take_due`),
  `page_did_become_ready` schedules every topic again, and the feed sends
  `recording` at once after `page.ready` (check 18).
- **The host's `# Threads` doc** gets the feed, the owner and decision 3's
  lock order.
- **The host tests that change** (`crates/steno-host/tests/main_window.rs`
  on `main`):
  - `recording_follows_the_recorder_and_the_start_selects_the_live_row`
    (1886) waits on `next_flush_due`, which goes; its fold case moves to
    check 21, and the rest waits for the feed's send;
  - `the_list_follows_the_recorders_state_and_meeting` (1949) expects the
    hook to reload the list, which the host crate no longer does; it moves to
    check 23;
  - `concurrent_publishes_reach_the_sink_in_build_order` (876) waits for the
    throttled `recording` through `next_flush_due`; it waits for the feed's
    last send instead.
- **Tests:** checks 1 and 2 (the settings-save cases), 16, 18, 19, 20, 21, 22
  and 23; check 12 keeps passing with the poll as the owner.

**As built** (PR 2c):

- **A third flag in the feed.** Beside "page ready" and "resend", a "poked"
  flag that the hook and the start set and each pass swaps; `page.ready`
  sets "resend" and unparks without it. A poke sends the status even
  when it did not move, throttled, so every hook still gets its snapshot
  (check 1: the synthetic tone's levels repeat exactly). A wake with
  neither flag set and no snapshot held back (the unpark of a poke an
  earlier pass already answered) sends nothing, so no duplicate goes
  out. It starts set, so the first pass reads the status even when a
  move came before the thread could be woken.
- **What goes at once.** A snapshot whose anything but the levels differs
  from the last one sent (the countdown and the denied permissions as
  well as the state, the meeting and the messages); otherwise it waits
  out the interval.
- **The owner is one function,** `own_the_reloads(host, every)` in
  `crates/steno-services/src/app.rs`, which the launch spawns with the
  2 s interval. Checks 19, 20, 22 and 23 drive it over a host directly;
  check 19's owner ticks every 50 ms and counts ten passes by their
  `settings.phone`.
- **The host's API.** `Host::reload_pass` is the pass; `store_changed`
  is the same pass forced, and runs the light part too.
  `Host::refresh_pairing` goes into the light part.
  `Host::reload_requested` is what the owner awaits beside its tick, and
  `Host::reload_requests` (hidden) counts the requests for check 23.
- **The in-memory mark** is the commit count alone: no other connection
  can reach a private in-memory database. The commit hook runs just
  before a commit is visible, so a mark can count a commit a read still
  misses; on a file the reader's `data_version` moves only once it is
  visible, so the next pass reloads again (the `Store::change_mark` doc).
- **The bounds.** "Live" is `LIVE` (1 s) in the services' checks as for
  2a, and check 20 waits one poll interval and `LIVE`; checks 16 and 21
  keep 100 ms. Check 23's stopped row is "out of `recording`": the
  pipeline may have taken it up before the pass ran.
- **Check 12** holds forced passes, since an idle tick no longer reloads:
  an event's request, then one the test makes. The burst test commits
  every 100 ms meanwhile, so each tick reloads.
- **Host tests that changed besides the three named:** `page.ready`'s
  order test, the doc example and the harness wait for the feed's first
  `recording`; the onboarding test of the system audio row's wait and the
  pairing test run a pass. `GatedPipeline` moved to the services' testing
  kit, whose `TimedSink` keeps the thread that sent each snapshot.
- **The recorder's hook rule** holds in the code as it is
  (`CaptureRecorder::notify` runs with the recorder's locks released);
  nothing in the recorder changed.

## Acceptance

Each check that needs the lock holds another connection's write transaction
(`WriteLockHold`, from #278) for 7 s. Each one runs on a runtime with one
worker, so a wait on that worker shows as a missed deadline. "Live" means the
observed thing happens within 200 ms during the hold. As built, "live" is 1 s
in the services' checks (200 ms in the store's checks 10 and 15), checks 2
and 4 add a 2 s bound from the user's side, and check 11 holds 2 s
(WP2a's and WP2b's as-built). Every check fails without its PR, on `main`,
except the guards (checks 17, 18, 20 and 22), which pass on `main` and fail on
a PR that breaks what they pin. The numbers are the plan's
earlier ones: check 14 is removed, check 9 moves whole to
`.plans/2026-10-15-host-actor.md`, check 6 is narrowed, check 18 is restated
as an outcome, and checks 19 to 23 are new.

| # | Delay from the audit | Check | Where | PR |
|---|---|---|---|---|
| 1 | Levels to the UI wait on the host lock | `levels_keep_flowing_while_another_process_holds_the_write_lock`: during a pipeline write's wait, and during a settings save's wait (the save stays under the view-model lock), each `recorder_changed` returns and its `recording` snapshot reaches the sink live | new `crates/steno-services/src/locked_store_tests.rs` | 2a, 2c |
| 2 | Recorder status, the `recording` topic, device notices and the disk watch's warnings (the same hook) | `a_device_notice_reaches_the_host_while_a_write_waits`: the notices thread's hook returns live, and the auto-stop's resume is armed on time, for the same two waits | same file | 2a, 2c |
| 3 | Auto-stop countdown | `an_auto_stop_under_a_held_lock_saves_once_the_lock_goes`: the countdown's stop shows `Stopping` live, and the meeting is `queued` within 1 s of the release | `crates/steno-services/src/auto_stop/tests.rs` | 2a |
| 4 | Stop and save | `a_stop_under_a_held_lock_shows_stopping_at_once_and_saves_once_the_lock_goes` (`Stopping` within `LIVE` of the stop's notice and within `END_TO_END` of the stop, `queued` within 1 s of the release), and `a_stop_saves_while_every_runtime_worker_is_blocked`, which pins decision 4 | `crates/steno-services/src/recorder.rs` tests | 2a |
| 5 | UI Stop button and command workers | `a_held_host_call_leaves_the_command_workers_free`: while a gated fake's call is held, another command on the same one-worker runtime answers | `apps/desktop/src-tauri/src/bridge.rs` tests | 1 (passes) |
| 6 | The UI's commands answer | `a_command_answers_while_a_write_waits`: `meetings.select` and a Settings read answer live during a pipeline write's wait. Narrowed: the case of another window's settings save moves to the host actor, since the save keeps the lock until then | `locked_store_tests.rs` | 2a |
| 7 | Tray and menu Record, Stop and Launch at login on the main thread | `the_trays_record_returns_before_the_stop_ends`, `the_trays_launch_at_login_returns_before_the_host_answers` and `the_tray_reads_stopping_while_the_stop_still_runs` (gated fakes); in click order on the lane, `two_quick_tray_clicks_reach_the_host_in_click_order`, `a_record_clicked_during_a_held_stop_runs_after_it`, `a_second_launch_at_login_click_undoes_the_first`, `two_window_requests_reach_the_host_in_the_order_they_came` and `the_lane_runs_its_jobs_in_order_and_past_a_panic`; the Quit, `a_quit_waits_for_the_stop_clicked_before_it`, `a_quit_goes_ahead_once_its_patience_has_passed` (within 1.5 s of a 200 ms patience), `a_quit_refuses_starts_and_retitles_the_tray_at_once`, `a_record_queued_before_a_quit_is_refused_before_the_exit`, `the_record_rows_read_quitting_once_quit_was_chosen`, `quit_sets_the_quitting_rows_and_the_snapshots_after_it_keep_them` and `only_a_snapshot_that_moves_a_row_sets_the_rows`, and in the services `a_start_after_a_menu_quit_is_refused_with_a_message`, `a_stop_under_way_at_a_menu_quit_still_tries_its_commit_again` and `the_pages_record_during_a_menu_quit_is_refused_and_says_why`; a request owed to a page before a later one, `an_owed_request_goes_out_before_a_later_one`; off the caller's thread, `a_window_request_is_published_off_the_callers_thread` and `the_onboarding_close_reaches_the_host_off_the_closing_thread`; `a_host_call_on_the_marked_ui_thread_asserts`, and one fixture-host test per shell entry point that fails without its assert (`a_command_…`, `a_page_load_…`, `a_window_request_…`, `the_onboarding_close_on_the_ui_thread_asserts`); and the measured smoke `apps/desktop/scripts/locked-store-linux.sh` (about 25 s on atlas), in `rust-ci.yml`'s Linux job as "Desktop stop under a locked store" | `actions.rs`, `windows.rs`, `main.rs`, `tray.rs` and the shell's `host.rs` tests, `crates/steno-host/src/ui_thread.rs` (the assert), the script | 1 (passes) |
| 8 | Start under the lock | `a_start_under_a_held_lock_is_refused_and_the_ui_stays_live`: after about 5 s the start is refused with "Steno could not create the meeting.", no capture opened, and checks 1 and 6 hold meanwhile | `recorder.rs` tests | 2a |
| 9 | Moved | A host write of the user's own under a held lock (the settings save, and the delete's undo) moves whole to `.plans/2026-10-15-host-actor.md`: those writes keep the view-model lock until the host actor | | |
| 10 | Detection's settings poll | `a_read_does_not_wait_behind_a_write_that_waits_on_another_connection` (the store, on the shared reader and on the host's), and `the_detection_poll_reads_the_settings_while_a_write_waits` | `crates/steno-core/src/store/mod.rs` tests, `crates/steno-services/src/detection/tests.rs` | 2a |
| 11 | Handover admit on a worker | `an_admit_under_a_held_lock_leaves_the_worker_free`: a timer ticks live during the wait, and the admit commits once the lock goes. As built the lock is held 2 s, not 7 s, and the timer may stall 600 ms (check 12's window): a 7 s hold runs out the admit's busy timeout (WP2b's as-built) | `crates/steno-pipeline/tests/store_waits.rs` | 2b |
| 12 | The 2 s poll and the events task | `the_poll_and_the_events_task_never_block_a_worker`: a timer task on the services runtime keeps ticking live while a forced reload is held at a gate (a gated fake pipeline's `export_keeps_failing`, which the detail's reload calls); `the_event_follower_takes_one_event_at_a_time` (no task per waiting event), `a_held_poll_is_not_followed_by_a_burst` (`MissedTickBehavior::Delay`) and `the_event_followers_sweep_never_blocks_a_worker` (the sweep's write held by another connection). 2c keeps them passing with the poll as the reload owner; since an idle tick no longer reloads, the held reload comes from a forced pass | `crates/steno-services/src/app.rs` tests | 1 (passes) |
| 13 | Pipeline runs on the workers | every `store_waits` test passes on one worker, including the quit during a run's wait | `store_waits.rs` | 2b |
| 14 | Removed | The deadlock test for the host write mutex, the emitter's leaf and the reader checkout, and the "view models held" assert. None of those exists in this cut. What is left of the order (decision 3) is checked by the nesting assert (check 15) and by checks 1, 2, 16 and 21, which hold the view-model lock while the feed and a stop run | | |
| 15 | The readers' own rules | `a_write_through_read_fails_on_both_stores` (file and in-memory, `query_only`, the host's reader included); `a_store_call_inside_a_store_closure_asserts`; `the_checkpoint_waits_for_reads_in_flight_and_holds_off_new_ones` (on both readers); `the_in_memory_checkpoint_takes_its_one_lock_once` (returns at once, as `checkpoint_store` at launch in the app tests needs) | `crates/steno-core/src/store/mod.rs` tests | 2a |
| 16 | The stop's hook behind a slow reload | `a_stop_during_a_slow_reload_shows_stopping_at_once_and_finishes_its_files`: a forced reload is held in flight with the view-model lock taken (a gated `export_keeps_failing`), then the recorder stops; `Stopping` reaches the sink within 100 ms, and the session's files finish while the reload is still held | `crates/steno-services/src/recorder.rs` tests | 2c |
| 17 | The delete's files and rows | `a_kill_after_the_entry_is_forgotten_keeps_the_meeting`, `a_kill_after_the_delete_commits_never_brings_the_meeting_back` and `a_kill_before_a_kept_meetings_entry_is_restored_keeps_it`: a child process deletes through the host, is killed at a gate between two steps, and the next launch lists the meeting with its recording, or does not list it and adopts no master, exactly once. A guard, not a fix: it passes on `main`, and runs with 2a, which changes the checkpoint that the forget and the undo fall back to | `crates/steno-services/src/kill_tests.rs` | 2a |
| 18 | `page.ready` against the page's first command | Restated as an outcome: `a_command_before_page_ready_loses_no_snapshot`: a command reaches the host before `page.ready`; once `page.ready` arrives, every topic, `recording` included, reaches the sink live, with the command's effect. Which of the two reaches the host first stays open (a probe saw the command first in 29 of 300 runs before PR 1 and 42 of 300 at its head). A guard: on `main` the `recording` the command's publish stamped arrives at most 50 ms late, within the 200 ms, so the check passes there and fails on a 2c that loses a topic | `crates/steno-host/src/recording_feed.rs` tests | 2c |
| 19 | An idle app reloads and resends every 2 s | `an_idle_app_sends_no_list_detail_or_progress`: with the page ready and no commit, ten polls send no `meetings.list`, `meeting.detail` or `progress` snapshot | `crates/steno-services/src/app.rs` tests | 2c |
| 20 | Another process's commit | `a_commit_from_another_process_shows_within_one_poll`: a second connection retitles a meeting, and the list carries the title within one poll interval and 200 ms; `an_in_process_commit_shows_within_one_poll` does the same through the in-memory store's writer (the commit count). A guard for the gate | `app.rs` tests, `crates/steno-core/src/store/mod.rs` tests (`change_mark`) | 2c |
| 21 | The feed's own rules | `the_recording_feed_sends_nothing_before_page_ready` (levels and a state change before `page.ready` reach no sink; the first snapshot goes out after it); `the_recording_feed_runs_while_the_view_models_are_held` (with the view-model lock held by a test, level updates reach the sink live and `Stopping` within 100 ms); `a_second_page_ready_gets_the_recording_again` (a reloaded page, with the recorder idle and its status unchanged, gets one `recording` snapshot); `a_held_level_update_stamps_no_interval` (a level update held back by the throttle neither becomes the last status sent nor restarts the 50 ms, and goes out when its interval ends); two level updates inside one interval fold into one send, which `recording_follows_the_recorder_and_the_start_selects_the_live_row` in `crates/steno-host/tests/main_window.rs` checks today through `next_flush_due` and checks here instead | `crates/steno-host/src/recording_feed.rs` tests | 2c |
| 22 | What the store does not show | `an_event_without_a_commit_still_reloads` (`StoreBusy`, `OperationFailed` and `ModelsMissing` with no commit reach the detail and the progress live); `a_permission_granted_outside_the_app_shows_within_one_poll` (the light part runs every tick). A guard for the gate | `app.rs` tests | 2c |
| 23 | One owner for the reloads | `reload_requests_during_a_pass_run_one_more_pass` (ten requests during a held pass run exactly one more forced reload; the count is of forced reloads, not passes, since a tick can land beside the stored permit); `a_stop_asks_for_a_reload_per_move` (`Stopping` and then `Idle` are two moves, and each asks for one; the stopped meeting's `queued` row reaches the list live after the save, from a forced pass, not from the stop's thread). The host test `the_list_follows_the_recorders_state_and_meeting` (`crates/steno-host/tests/main_window.rs:1949`) moves here: the host crate has no reload owner, so the list now follows through the services' poll task | `app.rs` tests, `recorder.rs` tests | 2c |

**Check 7's smoke** runs on the `xvfb-recording-linux.sh` harness, with the
stand-in tray host that `close-without-tray-linux.sh` uses. It records, holds
the lock for 8 s from `python3`'s `sqlite3`, and stops through the tray's menu
(`tray-watcher-linux.py click <bus name> <path> "Stop recording"`). The
page's Ctrl+Shift+R would go through `bridge_call` instead, and off the Mac
the tray has no shortcut. It passes when the menu's row reads "Stopping…"
within 3 s of the click, checked again once the read answered (each read starts
Python, 0.13 to 0.45 s on a busy host, so 1 s failed one run in five; the old
main-thread stop held the menu 8 s and more), the meeting is `queued` within 2
s of the release (the store's row: no log line marks a save),
`require_saved` holds, and the row reads "Record" again within 3 s after
that (a row the tray skipped as unchanged would stay "Stopping…"). It logs
each read of the menu with the times it began and answered, after the
click, so a slow run shows whether the app or the read was late.

- **Measured.** A whole run, the headless PipeWire's start included, took
  25.9 s on atlas (a debug build; the row read "Stopping…" 0.23 s after the
  click, and the meeting was queued 0.11 s after the release), and 24 s as a
  step of the hosted Linux job on PR 1's first run. That is under 2 minutes,
  so it joins `rust-ci.yml`'s Linux hosted job, beside
  `close-without-tray-linux.sh`, as "Desktop stop under a locked store".
  It runs by hand with
  `scripts/pipewire-headless.sh apps/desktop/scripts/locked-store-linux.sh target/debug/steno-desktop`.
- **Measured again with the lane and the 3 s bound** (atlas at a
  load of 17 to 21): five runs passed in 27.0 to 35.1 s, the row reading
  "Stopping…" 0.49 to 1.67 s after the click and the meeting queued 0.16 to
  0.38 s after the release; a sixth ended before its window showed, as GTK
  could not open the Xvfb display, a harness flake. Against `main`'s binary,
  before PR 1, the smoke fails as it should: the menu did not answer within
  3 s of the click, as the main thread was waiting on the stop's save.
- **The Quit during the held stop** (a probe, a Quit 0.5 s after
  "Stopping…"): with the lock held 8 s the app exited 7.2 s after the Quit,
  the meeting `queued` with the reason `manual`; held 13 s, past the Quit's
  10 s patience, it exited 11.3 s after the Quit, and the stop's last try,
  under way when the shutdown came, still saved it `manual`.

Not changed, and covered by the tests that already exist: the quit save on its
`ExitGate` thread, and the launch's recovery, which already runs on the
blocking pool (`kill_tests.rs`, and #278's quit tests).

## Risks

- **The user's own save under a held lock.** The host's writes stay under the
  view-model lock until the host actor. While another process holds the write
  lock, a settings save, a notes edit or a speaker confirm waits up to 5 s and
  then reports the busy error, and the other commands and topics wait with
  it. The levels, the `recording` snapshot and a stop do not (checks 1, 2, 16
  and 21). It happens only while another process writes: the Swift app in the
  rollback window, or a tool with the database open. Nothing is lost. The
  delete's undo stays under the lock as well (`ForgottenEntry::drop`,
  `host.rs:1665-1680`): one small durable write of the folder record on a
  refused delete, and a fallback checkpoint only when that write fails
  (`recorder.rs:2160-2199`), which can wait out the busy timeout with the
  lock held.
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
  that stays busy. A Record clicked before the Quit and still queued, or one
  from the page or the prompt during the wait, is refused
  (`Recorder::refuse_starts`), so no sub-second meeting is saved on the way
  out.
- **One ordered lane.** A job that waits holds up the jobs behind it. A deep
  link's or `window.open`'s request, the onboarding close and the tray's next
  action wait behind a tray Stop whose save waits (up to its three tries,
  15 s). The page's own commands do not: they stay concurrent on Tauri's
  blocking pool (check 5).
- **What the gate does not see.** A reload also reads things the store does
  not hold: whether the recording's files exist and the pipeline's in-memory
  flags (`detail.rs:120-156`: `files.exists`, `export_keeps_failing`,
  `damaged_audio`, `audio_may_be_damaged`), and the clock, through the
  default titles, which switch from a weekday to a date as the days pass
  (`display_title`, `labels.rs:97-108`). The pipeline's flags change with the
  events the follower forces a pass for (check 22). A recording's files
  removed outside Steno, or a default title crossing that boundary, show at
  the next reload instead of within 2 s. The Swift app followed only the
  database, through GRDB's observation, so this matches it.
- **The pairing poll still sends.** The light part's pairing refresh
  schedules `settings.phone` on every tick (`refresh_pairing`,
  `host.rs:937-948`), so an idle app still sends that one small snapshot every
  2 s. The change feed after stable can make it change-driven.
- **The feed's order against `page.ready`.** The first `recording` snapshot
  now comes from the feed's thread, after `page.ready`'s batch has gone out.
  The page takes topics in any order, but the host's tests that count the
  topics right after `page_ready` (the `Host` doc example, `host.rs:297`)
  wait for the feed. A command can still reach the host before `page.ready`
  (`usePageReady` does not await it); check 18 pins that the first paint
  loses nothing when it does.
- **Readers run out.** The shared reader serves the detection poll, the
  pipeline, the handover and the delivery. A long read (a search, an export)
  holds it, and other reads wait for it, never for a write; today they wait
  on the one connection that also writes. The nesting assert keeps a nested
  read from waiting on its own reader forever.
- **Ordering of events.** Within a pipeline task, each await keeps the write
  before the event that announces it. The follower requests its reload after
  it applied the event, and a pass reads the mark before it reloads, so a
  reload never misses a commit made before its request. Replies and snapshots
  were unordered already (`dispatcher.rs`), and the page does not rely on
  their order.
- **A dropped future's write still lands.** A quit or a `select!` that drops a
  facade future does not stop the closure. Code after such a drop must not
  assume the write did not happen. This is the same as a process killed
  mid-write, which recovery already handles. The phone admission goes
  further: its enqueue and the handover's receipt run in tasks of their own,
  so a dropped `complete` leaves a whole admission (WP2b's as-built).
- **The checkpoint under another process's lock** keeps both readers
  through `wal_checkpoint`'s busy wait, up to 5 s, so reads wait with it.
  The launch and the recorder's fallbacks when a folder record cannot be
  written or forgotten (a stop, a delete) are its only callers, so the UI
  meets it only in that fallback.
- **A copy left for the launch.** An admission whose commit and every
  `failed` save run out their busy timeouts leaves its copy, still noted,
  and the launch adopts it; a later retry that admits then gives a
  duplicate meeting, never a lost one. That needs another process to hold
  the lock for more than about 25 s over the wire (the `verifying` write,
  the commit and three tries of the save), as on `main` it needed about
  15 s.
- **The blocking pool.** Tokio's default is 512 threads; idle ones end after
  10 s. Each caller waiting on the writer's mutex holds one pool thread, and
  the app has a handful of such callers (the pipeline's one run, the delivery,
  the handover's uploads), far under the limit.
- **Swift parity.** None needed: no schema, no stored value, no Swift change.
  WAL is on in both apps, and the Swift app's pool already opens readers. The
  `hooks` feature adds no dependency. The commit hook cannot see the Swift
  app's commits; `PRAGMA data_version` does (check 20).

## What it does not do

The parts below come after stable, in `.plans/2026-10-15-host-actor.md`:

- It does not move the host's writes off the view-model lock, the delete's
  undo included. The host actor does, by taking the lock away.
- It does not add the change feed or targeted reloads. The writer's commit
  hook only counts commits, and a pass reloads everything or nothing.
- It does not change how the pipeline waits for models or for the store
  (`ModelWaits`, `pipeline.rs:477-655`, and its four resumes). The scheduler
  driven by the meeting rows does.
- It does not add a writer thread.

And, as before:

- It does not shorten or change the 5 s busy timeout, the stop's three tries,
  or #278's park and retry.
- It does not make the recorder's start, stop or quit async, and it does not
  restructure the recorder or the capture session. They stay on their own
  threads by design (decision 4), and D9 freezes audio at rc.1.
- It does not move the secret store's read out of the view-model lock. On the
  Mac that read can wait on a Keychain dialog, an existing hold that has
  nothing to do with SQLite.
- It does not make every `Store` method async. Synchronous callers on their
  own threads keep the synchronous API.
- It does not change the CLI's commands, P15's lock, or the database's
  journal mode.
