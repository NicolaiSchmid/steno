# The host actor, the change feed and a pipeline scheduler

After stable (`0.11.0`), owned by the pipeline, store and export area
(`wp-pse-*`). It supersedes the after-stable parts of
`.plans/2026-10-12-store-off-the-workers.md` (P40): the host's writes leaving
the view-model lock, the delete's undo among them, the change feed and the
targeted reloads. Once the host actor lands (PR 2 below), it also supersedes
that plan's decision 3 lock order. It follows Nicolai's decision of 2026-10-15 to take P40's smaller cut
for rc.1 and to do this work after stable. Line references are against `main`
at c79d00b70.

## What changes and why

The Swift app ran its controller and its bridges on the main actor, so every
call waited its turn and nothing needed a lock. The Rust port put the view
models behind `Mutex<Inner>` (`crates/steno-host/src/host.rs:209-245`), and
eight threads called into it. P40 takes three of them away: the levels thread
and the stop's thread now only poke the `recording` feed, and the flush
thread is gone. Five still do: the command workers, the shell's lane, the
main thread at launch, the services' reload owner and follower, and the
update and phone notifications. Each order the actor gave for free became a
rule or a fix: the shell's ordered lane, P40's lock order,
and the host's writes that wait under the lock while another process holds
the database. The pipeline has the same shape in its waits: `ModelWaits`
(`crates/steno-pipeline/src/pipeline.rs:477-655`) copies into memory what the
`queued` and `processing` rows already say, and the bugs found while #278
was built came from keeping that copy in step.

This plan gives the host one owner thread again, lets the store say what
changed, and lets the pipeline schedule from its rows.

## Decisions

### 1. The host actor

- **One thread owns `Inner`.** It runs on a `steno-host` thread. `Host` becomes
  a cheap handle over an `mpsc` sender, with the same public methods, so the
  shell and the services do not change.
- **The messages.** `Call { window, method, params, reply }`, `PageReady`,
  `StoreChanged { scope }`, `RecorderMoved`, `MeetingEvent`, and one per
  current `Host` notification (`updates_changed`, `phones_changed`,
  `request_meeting` and the rest).
- **Order is the channel's.** `bridge_call` awaits the reply, so no command
  worker blocks. The tray and deep links send without waiting, and the
  channel's order is the click order. That replaces the lane
  (`host::call_in_order` in the shell) for host calls, and most of
  `windows::Pages`' ordering.
- **Long work runs off the actor and posts back.** The recorder's start and
  stop, the probes, the prompts and the downloads already split this way
  (`outside_lock`, `host.rs:1328`). Dialogs run on the caller's side before
  the send.
- **Writes are jobs.** The actor reads a write's inputs, hands the write to the
  store (P40's `Store::writing`), and applies the result when `WriteDone`
  comes back, then replies. A second save sees the first one's result because
  the actor applies them in order. There is no host write mutex.
- **The delete's undo leaves the lock here.** `ForgottenEntry::drop`
  (`host.rs:1665-1680`) keeps the view-model lock in P40, on purpose: one
  small durable write of the folder record on a refused delete, and a
  fallback checkpoint only when that write fails
  (`crates/steno-services/src/recorder.rs:2160-2199`). The lock keeps another
  prompt's delete of the same meeting from committing between the row's read
  and the restore. On the actor the undo is a job like any write: the actor
  reads the row, the restore runs off it, and the actor keeps the meetings
  whose undo is in flight and holds a delete of one until its `WriteDone`
  comes back. Only the actor touches that set, so it needs no lock. The
  delete itself already runs its forget off the lock (`host.rs:1786-1797`).
- **The `recording` feed stays** as P40 built it. The hook still pokes it; a
  move posts `RecorderMoved` instead of setting the reload flag.
- **What it removes:** the publishing mutex, `Inner`'s mutex, the reload flag
  and its `Notify`, the shell's `call_blocking` for host calls and the lane
  for host calls, and every rule of P40's decision 3 except the store's own
  (no store call inside a store closure, and a commit hook that takes no
  lock). With `recording` in its own feed, no topic is throttled, so the
  actor has nothing to flush on a timer: it publishes at the end of each
  message.

### 2. The change feed

- **In-process commits.** A `commit_hook` and an `update_hook` on the writer
  connection collect the tables and rows a transaction touched, and the
  commit posts `StoreChanged { meetings, settings }`. P40's commit count stays
  as the hook's fallback for the in-memory store.
- **Other processes.** The actor reads `PRAGMA data_version` on its own reader
  at each tick; a move is a full scope, since the hooks see only this
  process's commits.
- **What the hooks cannot see.** Some of the full-text shadow tables (the
  `_idx` and `_config` ones) are `WITHOUT ROWID`, which `update_hook` skips;
  they follow their content tables, so nothing is lost.
- **Targeted reloads.** A meeting row reloads that meeting's list row and, when
  it is open, its detail. The speaker and person tables reload the list's
  chips. Settings reload the sections. An empty scope reloads nothing.
- **The poll's light part stays** for what the store does not hold (the
  pairing, the permissions, the models), and `settings.phone` goes out only
  when the pairing refresh changed it.

### 3. A pipeline scheduler driven by the meeting rows

- **One entry point**, `Scheduler::wake(reason)`, with the reasons launch,
  model installed, store retry timer, commit and Process again.
- **Each wake scans** the `queued` and `processing` rows. It skips what
  `InFlight` holds, checks the preconditions (the models present, the store
  free), claims the meeting and runs it.
- **"Waiting for models" and "waiting for the store"** become an informational
  map for the card and the list, not control flow. The texts #278 added stay
  word for word.
- **What it removes:** `ModelWaits`' sets (`waiting`, `for_store`), the resume
  counter, `newest`, `unfinished_owed`, and the four resume variants
  (`resume_unfinished`, `resume_waiting`, `start_waiting`,
  `resume_waiting_for_store`, `pipeline.rs:1489-1616`). About 600 lines go and
  250 come in.
- **No schema change.** Both apps already have the states.

### 4. A writer thread, only if needed

`Store::writing`'s body moves to one writer thread only if the blocking pool
shows pressure in a measurement. Reads never go through it, so P40 decision
1's objection (reads queued behind writes) does not apply.

## PRs, in order

| PR | What | Size | Risk |
|---|---|---|---|
| 1 | The actor behind the unchanged `Host` API, running the existing `&mut Inner` bodies on its thread | About 1,500 lines, mostly mechanical | Medium |
| 2 | Writes as jobs, applied on completion, the delete's undo included; the publishing and `Inner` mutexes, the lane for host calls and `call_blocking` for host calls go; P40's decision 3 is superseded | About 1,000 lines | Medium |
| 3 | The change feed and targeted reloads | About 400 lines | Medium to low |
| 4 | The pipeline scheduler | About 600 lines out, 250 in | Medium |
| 5 | The writer thread, only if measured | Small | Low |

The recorder and the capture session are not in this plan: they have their
own after-stable order (the test seam and fake, the recorder thread, then the
capture session, which needs a new D9 ruling).

## Acceptance

Every PR keeps the bridge fixtures in `apps/macos/web/fixtures/bridge/` byte
for byte, and every P40 check that still applies passes unchanged.

- **PR 1.** The host's bridge tests and fixtures pass with no edit. P40's
  lane tests (check 7: click order, the Quit's wait, an owed request
  before a later one) pass against the actor's inbox. A command and a level
  update at once never deadlock, by construction: one thread holds `Inner`.
- **PR 2.** P40's check 6 comes back whole: a settings save under a held lock
  leaves another window's commands live. P40's check 9 comes here whole: the
  save waits, then reports the busy error, while the list, the levels and
  another command stay live, and the same holds for a refused delete's undo
  whose fallback checkpoint waits. P40's check 17, the delete's kill tests,
  passes unchanged. Two quick saves apply in order.
- **PR 3.** An idle app sends no snapshot at all, `settings.phone` included; a
  commit to one meeting reloads only that meeting's row and detail; another
  process's commit shows within one tick (P40 checks 19 and 20 hold).
- **PR 4.** `crates/steno-pipeline/tests/store_waits.rs` and
  `model_waits.rs` pass with no edit to their expectations, and so do the kill
  tests (`crates/steno-services/src/kill_tests.rs`). A meeting queued at
  launch with its models missing starts once the model installs; one parked
  for the store starts on the retry timer's wake; Process again on a parked
  meeting runs it once.
- **PR 5.** A measurement before and after, on the blocking pool's thread
  count during a pipeline run under a held lock.

## Risks

- **A message that waits holds up the ones behind it.** Long work must stay
  off the actor; PR 1 names every body that blocks (the secret
  store's read in `reload_sections` among them) and moves it out.
- **Replies and snapshots in a new order.** The page treats every snapshot as a
  full state, so the order across topics does not matter; within a topic the
  actor's order is the build order.
- **The scheduler claims from the rows.** A claim must be the same write the
  run makes today (`queued` to `processing` only over its own state), so two
  pipelines sharing `InFlight` never run one meeting twice.
