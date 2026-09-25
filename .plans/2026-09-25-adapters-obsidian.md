# Steno adapters: destination runtime and the Obsidian folder destination

Status: implementation plan, 2026-09-25, reconciled and then revised the same day after the three
reviews (program review application log). Program:
[`2026-09-25-v1-program.md`](2026-09-25-v1-program.md). Scope authority:
[`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md). Owns `Sources/StenoAdapters`,
`Tests/StenoAdaptersTests` and the `steno deliver` command. The scope's original task line did not
parse in the Obsidian Tasks plugin (it reads fields from the end); the corrected order
`- [ ] text [[Assignee]] ⏫ 📅 YYYY-MM-DD` is now in the scope as an erratum.

## Goal

Turn a `.ready` meeting into files on disk, reliably and repeatably. Three layers: pure renderers
from the canonical model to bytes (folder note, transcript, tasks, WebVTT, JSON, person page); a
`DeliveryCoordinator` that runs every configured `Destination` sequentially with one `Delivery` row
per meeting and destination and hands each its previous receipt; and one concrete destination, the
Obsidian vault folder, laid out as the scope says and constructed from core's typed
`ObsidianSettings`. Re-export rewrites only files the app wrote (tracked in a receipt) and never
deletes anything. Renderers know nothing about vaults, so a later WebDAV or Drive destination reuses
them with its own `RenderOptions`.

## Non-goals

- WebDAV, Google Drive, Nextcloud, webhooks, Notion, CRMs, MCP, REST (seam designed, not built).
  SRT, PDF, DOCX, HTML.
- Reading the vault back, two-way sync, watching for edits, git, `.obsidian/`.
- User-editable note templates or folder path templates (only vault path, people folder, audio
  opt-in and task tag are configurable); retry scheduling or background queues (explicit re-export
  is the retry); per-destination re-export (no UI in scope; `ProcessingPipeline.redeliver` re-exports
  to every configured destination).
- Transcoding audio: `AudioAsset.mixdownURL` is produced by the pipeline persist stage
  (`2026-09-25-audio-capture.md`); this module copies the file. Rendering the summary: core's
  `SummaryMarkdown.render(export)` produces the Markdown this module embeds.
- Deleting or moving vault files for any reason, including audio retention (once `audio.m4a` is in
  the vault it belongs to the user); renaming the meeting folder when a summary re-run changes the
  title (pinned at first delivery).

## Research notes

Verified 2026-09-25 against Tasks user guide, Obsidian help, Dataview docs, W3C WebVTT and YAML
1.2.2. Tasks: 🔺 highest, ⏫ high, 🔼 medium, none normal, 🔽 low, ⏬ lowest; `📅 YYYY-MM-DD` due;
fields follow the description and only tags and block links may follow them; NBSP and U+FE0F break
recognition, so Steno emits bare code points. Obsidian properties: Date `2020-08-21`, Date & time
`2020-08-21T10:30:00`, Number, List, Tags (no `#`); `tags`, `aliases`, `cssclasses` are defaults;
wikilinks quoted. Dataview: keys lowercased with spaces as dashes; ISO text is a date only with `T`;
durations are text like `1h 30m`. Folder notes: `Folder/Folder.md` is the default of the xpgo and
LostPaul plugins. WebVTT: `hh:mm:ss.ttt`; `<v Name>text` may omit `</v>` as sole span; text escapes
`&` `<`; annotations may not contain `&` `>` newlines; `NOTE` blocks allowed; no `-->` in payloads.
YAML: plain scalars cannot start with an indicator or contain `: ` or ` #` and are implicitly typed;
double-quoted scalars escape `\` `"` `\uXXXX`.

Unverified, checked in step 9: `yes`/`no` as booleans in Obsidian's parser (moot, strings are
quoted); Dataview parsing `"1h 30m"` as duration (moot, integer minutes); quoted `"[[Name]]"` list
entries as real links in graph and backlinks; Tasks recognising `⏬` without U+FE0F.

## Public API (`Sources/StenoAdapters`)

```swift
// Rendering: pure, deterministic, no I/O, no clock. Nothing renders the current time.
public struct RenderOptions: Sendable, Equatable {
    public var linkStyle: LinkStyle          // .none plain names, .wikilink
    public var peopleFolder: String?         // "People"; nil disables person links and pages
    public var taskTag: String?              // "task" for vaults with a Tasks global filter
    public var timeZone: TimeZone            // dates in frontmatter, info line and person lines
    public static let plain: RenderOptions   // .none, no people, no tag, UTC
}
public enum LinkStyle: Sendable { case none, wikilink }
public struct RenderedArtifact: Sendable, Equatable { public var kind: Kind; public var fileName: String; public var data: Data; public var personID: UUID?
    public enum Kind: String, Sendable { case folderNote, transcript, tasks, vtt, json, personPage, audio } }
public struct ArtifactRenderer: Sendable {
    public init()
    public func render(_ export: MeetingExport, options: RenderOptions) throws -> [RenderedArtifact]
    public func renderFolderNote(_ export: MeetingExport, options: RenderOptions) -> String     // embeds SummaryMarkdown.render(export)
    public func renderTranscript(_ export: MeetingExport, options: RenderOptions) -> String
    public func renderTasks(_ export: MeetingExport, options: RenderOptions) -> String
    public func renderVTT(_ export: MeetingExport) -> String
    public func renderJSON(_ export: MeetingExport) throws -> Data                             // StenoJSON.encoder().encode(export), pretty printed
    public func renderPersonLine(_ export: MeetingExport, folderSlug: String, options: RenderOptions) -> String
}
public struct Frontmatter: Sendable, Equatable {
    public enum Value: Sendable, Equatable { case string(String), int(Int), bool(Bool), date(Date), dateTime(Date), list([String]) }
    public var fields: [(key: String, value: Value)]   // insertion order kept
    public func encoded() -> String                    // "---\n…---\n"; every string double-quoted
}
public enum Slug {
    public static func title(_ s: String, maxLength: Int = 60) -> String   // "produktstrategie-90-10"; folding with locale: nil
    public static func fileName(_ s: String) -> String                     // strips / \ : * ? " < > | # ^ [ ] and controls
}
public enum MeetingFolder { public static func path(for meeting: Meeting, timeZone: TimeZone) -> String }   // "Meetings/2026-09-24-<slug>"
public enum Timecode { public static func clock(_ t: TimeInterval) -> String; public static func vtt(_ t: TimeInterval) -> String }  // "00:12:34", "00:12:34.567"

// Runtime. DeliveryDispatcher, DeliveryReceipt, Destination and ObsidianSettings are StenoCore types (program).
public actor DeliveryCoordinator: DeliveryDispatcher {
    public init(store: MeetingStore, settings: SettingsStore,
                destinations: @Sendable (Settings) -> [any Destination] = StenoAdapters.destinations(for:),
                now: @Sendable () -> Date = Date.init)
    public func deliverAll(meetingID: UUID) async -> [Delivery]   // never throws; reads Settings per call; passes each destination's stored receipt as previous
}
public func destinations(for settings: Settings) -> [any Destination]   // [ObsidianFolderDestination(settings:)] when settings.obsidian != nil, else []

// Obsidian.
public struct ObsidianFolderDestination: Destination {
    public static let destinationID = "obsidian-folder"
    public init(settings: ObsidianSettings, fileManager: FileManager = .default)
    public var id: String { get }
    public func validate() async throws
    public func deliver(_ meeting: MeetingExport, previous: DeliveryReceipt?) async throws -> DeliveryReceipt
}
public enum ObsidianError: Error, Sendable, Equatable { case vaultMissing(String), vaultNotWritable(String), peopleFolderInvalid(String), writeFailed(path: String, underlying: String), audioUnavailable }
```

`DeliveryReceipt` (root = vault path at delivery, folder pinned at first delivery, files with
`.owned` or `.managedBlock` ownership and sha256, `rendererVersion` bumped when output changes) and
`ObsidianSettings` (vaultPath, peopleFolder?, includeAudio, taskTag?) are StenoCore types;
`RenderedArtifact` is this module's.

## Default folder layout and formats

Slug. `Slug.title` lowercases, transliterates ä→ae ö→oe ü→ue ß→ss, strips other diacritics
(`folding(options: [.diacriticInsensitive, .caseInsensitive], locale: nil)`, machine-independent),
replaces runs outside `[a-z0-9]` with one hyphen, trims hyphens, cuts to 60 characters at the last
hyphen, falls back to `meeting`. Folder: `MeetingFolder.path`, always `Meetings/<yyyy-MM-dd>-<slug>`
with the date from `startedAt` in `RenderOptions.timeZone` (the scope's layout; no user template).
The folder basename is the slug used in file names. Collision on initial delivery: a folder whose
`meeting.json` has another `meeting.id` (or none) gets `-2`, `-3`, … appended; one with our id is a
crashed attempt and is reused.

```
<vault>/Meetings/2026-09-24-produktstrategie-90-10/
  2026-09-24-produktstrategie-90-10.md              folder note: frontmatter, summary, decisions, scratchpad
  2026-09-24-produktstrategie-90-10 - Transcript.md
  2026-09-24-produktstrategie-90-10 - Tasks.md
  transcript.vtt
  meeting.json
  audio.m4a                                          only with includeAudio
<vault>/People/Anna Müller.md                        only with peopleFolder; managed block only
```

Frontmatter (folder note) comes from `Frontmatter.encoded()`, never from interpolation; every
string is double-quoted with `\\`, `\"` and controls escaped, so colons, quotes, `#` and leading `-`
in titles are safe:

```yaml
---
title: "Produktstrategie: \"90/10\" & Roadmap"
date: 2026-09-24T14:00:00      # Obsidian Date & time, Dataview date; RenderOptions.timeZone, no offset
duration: 90                   # whole minutes, rounded up; Obsidian Number
participants:
  - "[[Anna Müller]]"          # plain "Anna Müller" when peopleFolder is nil; "Speaker 2" unlinked
tags:
  - meeting                    # always present
  - kunde/acme                 # user tags: no #, spaces to -, only [A-Za-z0-9_/-]
source: "mac-call"             # mac-call | mac-in-person | phone
template: "default"
language: "de"                 # LanguageTag.rawValue
steno_id: "0d6f…"              # lowercase UUID (YAML key stays snake_case, Obsidian convention)
---
```

`title` is added to the scope's list because the filename is a slug. Body: `# Title`; info line
`2026-09-24 14:00–15:30 · 1 h 30 min · Mac call · [[<slug> - Transcript|Transcript]] · [[<slug> -
Tasks|Tasks]]`; `## Summary` (`SummaryMarkdown.render(export)` verbatim, headings at level 2+, current
speaker names); `## Decisions` (bullet per `Decision.text`) and `## Scratchpad` (verbatim), each
omitted when empty. Speaker names everywhere come from `export.displayName(forSpeaker:)`.

Transcript note. Frontmatter `title`, `steno_id`, `type: "transcript"`. One `## Anna Müller —
00:12:34` header per turn (consecutive segments by one speaker; name wikilinked with people pages on;
`Timecode.clock` of the first segment), blank line inside a turn at gaps of 3 s or more; a paragraph
starting with `#`, `-`, `>` or `1.` is backslash-escaped.

Tasks note. Frontmatter `title`, `steno_id`, `type: "tasks"`. One line per `MeetingTask`, fixed
order: text, `[[assigneeName]]` if any, `#taskTag` if set, priority emoji, due date:

```
- [ ] Angebot an ACME schicken [[Anna Müller]] #task ⏫ 📅 2026-10-01
- [x] Protokoll verteilen 🔽
```

Priority from `MeetingTask.priority`: `.high` → `⏫`, `.normal` → none, `.low` → `🔽`; `🔺 🔼 ⏬`
unused with three levels (highest/lowest if the model grows). `done` writes `- [x]` without `✅` (no
completion date in the model). Newlines become spaces. Closing line: "Edit tasks in Steno; this file
is rewritten on re-export."

WebVTT. `WEBVTT - Steno <steno_id>`, a `NOTE` block with title and date, one cue per
`TranscriptSegment` in start order, `Timecode.vtt(start) --> Timecode.vtt(end)` (end at least start +
1 ms), payload `<v Anna Müller>text`. Text escapes `&` `<` `>`, drops CR LF `-->`; annotation drops
`&` `>` CR LF.

JSON. `meeting.json` is `MeetingExport` through `StenoJSON.encoder()` with `.prettyPrinted` added:
camelCase, sorted keys, ISO 8601 with fractional seconds, `schemaVersion: 1`, no embeddings (they
have no CodingKey), `audio` carrying the asset or null. Byte-identical to `steno export` and
decodable by `StenoJSON.decoder()` as `MeetingExport`. Nothing in it depends on when it was written,
so an unchanged meeting re-exports to unchanged bytes and the receipt hash is stable.

Person page `People/<Slug.fileName(displayName)>.md`, display name as filename so `[[Anna Müller]]`
resolves. Created when missing with frontmatter (`steno_person_id`, `email?`, `type: "person"`) and
`# Name`. The app owns only the region between `<!-- steno:meetings:start -->` and `<!--
steno:meetings:end -->` (appended when missing): one line per meeting, newest first, `- 2026-09-24
[[<folder slug>|<title>]] %%steno:<meeting uuid>%%`; the line for this uuid is replaced or inserted,
bytes outside the markers copied unchanged. Renamed people get a new page; the old one stays.

Audio. `AudioAsset.mixdownURL` copied byte for byte to `audio.m4a` when `includeAudio` is on; a nil
`mixdownURL` yields `.audioUnavailable` after all other files are written.

## Delivery runtime

`deliverAll` loads `Settings`, builds `destinations(for:)`, calls `MeetingStore.export(meetingID:)`,
then per destination in order: save the `Delivery` row as `.pending` with `lastAttemptAt = now()`;
call `deliver(export, previous:)` with that destination's stored receipt (nil on first delivery or
after the row lost its receipt); save `.delivered` plus receipt, or `.failed(String(describing:
error))`; move on. Strictly sequential; one failure never blocks the next. There is no separate
re-export path: `ProcessingPipeline.redeliver(meetingID:)` calls `deliverAll` again.

Obsidian `deliver`: pick the folder (receipt on re-export, `MeetingFolder.path` plus collision rule
on first delivery); render once with `RenderOptions(linkStyle: .wikilink, peopleFolder:, taskTag:,
timeZone: .current)`; remove stale `.steno-tmp-*`; write each artefact through `AtomicFileWriter`
(temp `.steno-tmp-<name>-<8 hex>` in the target folder, `fsync`, `rename(2)`); merge person blocks
via `ManagedBlock`; return the receipt. On re-export only paths in the previous receipt or freshly
rendered are written; a file no longer produced (audio opted out, people off) stays on disk and in
the receipt. Files the app never wrote are never opened for writing; nothing is deleted except the
app's own temp files. `validate`: vault path is a writable directory (probe `.steno-probe-<hex>`
created and removed); people folder relative without `..`; missing `.obsidian/` is a warning, not an
error.

Future WebDAV or Drive destination: conforms to `Destination`, is constructed from its own typed
settings struct in core's `Settings`, calls `ArtifactRenderer.render(export, options:)` with its own
`RenderOptions` (likely `linkStyle: .none`), resolves the folder with `MeetingFolder`, pushes each
`RenderedArtifact.data` to `folder/fileName` through its transport, and returns a `DeliveryReceipt`
whose `root` is the remote base URL; `destinations(for:)` gains one line. The Obsidian destination
already writes through a small non-public `FileSink` with one `LocalFolderSink`; a `WebDAVSink` would
be the second implementation, at which point `FileSink` goes public per the two-implementations rule.

## Files

```
Sources/StenoAdapters/
  Rendering/ArtifactRenderer.swift, RenderOptions.swift, RenderedArtifact.swift   entry point; options and LinkStyle; artefact value
  Rendering/Frontmatter.swift, MarkdownEscaping.swift        YAML emitter (no dependency); escaping, wikilinks, tag sanitiser
  Rendering/FolderNoteRenderer.swift, TranscriptMarkdownRenderer.swift, TasksMarkdownRenderer.swift
  Rendering/WebVTTRenderer.swift, MeetingJSONRenderer.swift, PersonPageRenderer.swift, Timecode.swift   (JSON renderer is StenoJSON + prettyPrinted)
  Naming/Slug.swift, MeetingFolder.swift                     slug table and sanitiser; fixed folder path
  Runtime/DeliveryCoordinator.swift, Destinations.swift      coordinator; destinations(for:)
  Obsidian/ObsidianFolderDestination.swift, ObsidianLayout.swift, ManagedBlock.swift
  FileSystem/FileSink.swift, AtomicFileWriter.swift          internal sink protocol; temp + fsync + rename
Sources/steno/Commands/DeliverCommand.swift                  `steno deliver <meeting-id>`: ProcessingPipeline.redeliver with the real coordinator
Tests/StenoAdaptersTests/
  one <Type>Tests.swift per public type above, plus ManagedBlockTests, AtomicFileWriterTests, RendererVersionTests,
  ObsidianDestinationIntegrationTests, DeliveryCoordinatorTests; fixture via StenoCore `Fixtures.url`, goldens via `Snapshot`
Tests/Fixtures/meetings/produktstrategie.json                synthetic MeetingExport (step 1), sequential UUIDs, fixed dates
Tests/Fixtures/snapshots/obsidian/*                          one golden file per renderer output and variant; VERSION holds rendererVersion + sha256 of the goldens
Tests/Fixtures/snapshots/e2e/*                               vault goldens for StenoEndToEndTests (step 8)
Package.swift                                                adds StenoAdapters (depends on StenoCore only) and its test target; no third-party packages
```

## Steps

Each step is at most one day; checks run as `swift test --filter StenoAdaptersTests.<Name>` on the
`macos-15` CI job and are `[ci]` unless tagged otherwise. Every filesystem test uses a fresh temp
directory.

1. Target, fixture, naming. Fixture: title `Produktstrategie: "90/10" & Roadmap für Q4`; three
   participants, one speaker `.unknown` labelled `Speaker 2`, one `.confirmed`; 14 segments over both
   lanes, one containing `<`, `&`, a leading `# ` and `-->`; tasks covering all priorities, with and
   without due date and assignee, one done; two decisions; a `SummaryDocument` with two sections whose
   bullets mention `Speaker 2` and a confirmed name; scratchpad containing `---` and `## Summary`;
   tags `Kunde ACME`, `#q4`; all ids sequential, all dates fixed. Implement `Slug`, `MeetingFolder`,
   `Timecode`. Check: `SlugTests` (umlauts, ß, é, emoji-only → `meeting`, 61-char cut, forbidden
   characters, identical output under `de_DE` and `en_US` process locales), `MeetingFolderTests`
   (date in `Europe/Berlin` and `UTC`), `TimecodeTests` pass.
2. Frontmatter emitter, Markdown escaping, tag sanitiser. Check: `FrontmatterTests` golden plus
   cases for `: `, `"`, `\`, leading `-`, ` #`, `yes`, `2026-09-24`, empty string, control
   character; a round trip through `/usr/bin/ruby -ryaml -e 'YAML.load(STDIN.read)'` returns the
   same keys and string values.
3. Folder note, transcript, tasks renderers, `.plain` and `.wikilink`, `Europe/Berlin` and `UTC`.
   Check: golden files match; the folder note's `## Summary` equals `SummaryMarkdown.render` output
   with `Speaker 2` left as a label and the confirmed name in bold; tasks test asserts by regex that
   each line ends with the emoji fields, has no U+FE0F or U+00A0, `.normal` no emoji.
4. WebVTT and JSON renderers. Check: golden files match; VTT test asserts header, ordering, `end >
   start` for a zero-length segment, escaping of the `<`/`&`/`-->` segment; JSON test decodes the
   bytes with `StenoJSON.decoder()` back to an equal `MeetingExport`, asserts no `embedding` key,
   camelCase keys only, and byte equality with a second encode.
5. `AtomicFileWriter`, `LocalFolderSink`, `ManagedBlock`, receipt SHA-256. Check: no `.steno-tmp-*`
   left, content matches; read-only target yields `.writeFailed` without residue; `ManagedBlockTests`
   cover markers appended when missing, bytes outside preserved, line replaced by uuid, newest first.
6. `ObsidianFolderDestination` first delivery and `validate`. Check: delivery into a temp vault
   yields exactly the six-file layout, each file equal to its golden, receipt with five owned files
   (six with audio) plus two managed blocks; `validate` rejects a missing path and an unwritable
   directory with typed errors, accepts an empty directory.
7. Re-export, collision, preservation. Check: after the test adds `notes.md`, edits the person page
   around the block and changes the title, `deliver(_, previous: receipt)` leaves `notes.md` and the
   edits intact, keeps the folder name, updates `title`, keeps `audio.m4a` after `includeAudio` is
   off; an unchanged meeting re-exports to byte-identical files and hashes; a second meeting resolving
   to the same folder gets `-2`; a first delivery for the same `meeting.id` reuses the folder.
8. `DeliveryCoordinator`, `destinations(for:)`, `DeliverCommand`, end-to-end. Check:
   `DeliveryCoordinatorTests` with two fake destinations: first throwing → `.failed` with error text,
   second `.delivered` with receipt, `lastAttemptAt` on both from the injected `now`; a second
   `deliverAll` passes the stored receipt back as `previous`; `Settings.obsidian == nil` yields no row;
   `RendererVersionTests`: sha256 over all goldens in `snapshots/obsidian/` equals the hash recorded
   beside the version in `snapshots/obsidian/VERSION` (a golden diff with an unchanged version fails);
   `stenoTests`: `steno process` then `steno deliver <id>` with `Settings.obsidian` pointing at a temp
   vault writes the six files; `Tests/StenoEndToEndTests` swaps `FakeDestination` for
   `ObsidianFolderDestination` into a temp vault and compares every file with `snapshots/e2e/`.
9. `[manual]` check in a real vault with Tasks, Dataview and Folder Notes: folder note opens on
   folder click; properties show `date` as Date & time, `duration` as Number, `participants` as
   links; a Tasks `not done` query lists the fixture tasks with due dates and priorities; Dataview
   `FROM #meeting` shows the note; `[[Anna Müller]]` resolves; `transcript.vtt` plays in VLC. Record
   results for the unverified items.

## Tests

- Unit `[ci]` (steps 1 to 5, 8; no network, no real audio) and golden snapshots under
  `Tests/Fixtures/snapshots/obsidian/`: folder note plain/wikilink × Berlin/UTC, transcript
  plain/wikilink, tasks plain/wikilink/with tag, VTT, JSON, new person page, merged block. Failure
  prints a unified diff; `STENO_UPDATE_SNAPSHOTS=1 swift test` rewrites, reviewed in the PR together
  with the `VERSION` bump.
- Filesystem integration `[ci]` in a fresh temp directory per test: first delivery, re-export,
  user-file preservation, collision, crash reuse, audio opt-in with a 100-byte fake `.m4a`.
  Coordinator integration: in-memory GRDB store from core plus fake destinations. End-to-end: the
  shared target. Manual `[manual]`: step 9.

Reviewer trap: a changed golden in `snapshots/obsidian/` without a `VERSION` bump; any edit to the
"never deletes" assertions when `ObsidianLayout` changes.

## Spikes

- S1 (before step 3, half a day, `[manual]`): in a real vault, Tasks recognises the emitted line order
  with the wikilink before the emojis and `🔽`/`⏫` without U+FE0F. Failure changes the line order, not
  the architecture.

## Needs from other workstreams

- Core foundation: `MeetingExport` with `schemaVersion` and `displayName(forSpeaker:)` from
  `MeetingStore.export(meetingID:)`, `StenoJSON`, `SummaryMarkdown.render`, `DeliveryReceipt`,
  `Delivery.receipt`, `Destination` and `DeliveryDispatcher` as in the program, `ObsidianSettings` on
  `Settings.obsidian`, `ProcessingPipeline.redeliver`, `AudioAsset.mixdownURL`, the `steno` root
  command, `Fixtures` and `Snapshot` from `Testing/`, `Tests/StenoEndToEndTests`.
- LLM (`MeetingSummarizer` output): `SummaryDocument` sections with non-empty leads; task and
  decision text single-line.
- Speech (`Diarizer`): stable `clusterLabel` so `Speaker N` numbering is deterministic across
  re-exports.
- macOS app: settings UI editing `Settings.obsidian` (vault picker, people folder, audio opt-in,
  task tag) and calling `ObsidianFolderDestination(settings:).validate()`; delivery status from
  `observeDeliveries` and a re-export button calling `ProcessingPipeline.redeliver`; `ObsidianError`
  messages shown verbatim.

## Deferred

- User-configurable folder path template (`{{year}}`, `{{title}}`, ...); the folder is the scope's
  fixed `Meetings/<date>-<slug>`.
- Per-destination re-export once a second destination exists.
- Renaming the meeting folder when a summary re-run changes the title.

Follow-ups from the PR #7 reviews (each names the finding it comes from):

- Core: `MeetingSource` needs one wire key (`"mac-call"`, `"mac-in-person"`, `"phone"`) that
  `FolderNoteRenderer.sourceKey` and the CLI's `--source` map both read, so the next destination does
  not add a third copy (elegance 15).
- Core, privacy: `meeting.json` embeds `audio.mixdownURL` as an absolute `file:///Users/<name>/…`
  URL, which puts the account name and folder layout into a vault that is often synced. Encode it
  relative to the audio folder or drop it from `MeetingExport`'s JSON (correctness 12).
- Escaping `%%` (Obsidian comment) and `<!--` in transcript, task and decision text so a segment
  cannot hide the rest of the note in reading view. `&lt;!--` is safe CommonMark; whether `\%\%`
  stops Obsidian's comment parser needs the step 9 vault check before it lands (correctness 7).
- Person pages whose display names differ only in case, forbidden characters or Unicode
  normalisation map to one file on APFS; dedupe by normalised file name within one delivery and
  report the duplicate (correctness 9).
- Two meetings delivering at once can drop one line from a shared person page: the pipeline's lock
  is per meeting. A per-destination or per-file lock in the coordinator (correctness 10).
- An iCloud-evicted person page is `.Name.md.icloud`; treat the placeholder as existing and refuse
  the write instead of producing a conflict copy (correctness 11).
- When `deliver` throws after writing files (today only `audioUnavailable` on a first delivery with
  no audio anywhere), the files written are not in any receipt until the crashed-attempt rule
  recovers the folder on the next run. Returning the partial receipt alongside the error needs a
  `Destination` contract change (correctness 2, second half).
- `DeliveryCoordinator` could be a `struct` (no mutable state) and its two `try? store.save` could
  surface as `.failed` rows; the plan's `public actor` shape is kept until the app wires it
  (elegance 10). `Slug.fileName` is not a slug and could be `FileName.sanitized` (elegance 7);
  `noteHead`/`stenoID`/`orderedSegments` could leave the facade (elegance 6); tests named with
  "And" could be split (elegance 16). All cosmetic, none changes bytes.

## Deviations (implementation)

Recorded 2026-09-25 while building the plan in `feat/adapters-obsidian`. Each line names what the
code does differently from the text above and why; none widens the scope.

- `Frontmatter` is not `Equatable` (nothing compares two) and carries a `timeZone` for `.date` and
  `.dateTime`. Tags are *not* the plain scalars the layout above shows: `FolderNoteRenderer`
  sanitises them with `MarkdownText.tag` and the emitter double-quotes them like every other string,
  so a tag of `2026`, `true` or `null` stays a string (Obsidian reads quoted list entries as tags).
  `quoted` is internal.
- The render seam is split by placement instead of one `render` returning a mixed list:
  `renderMeetingFiles(_:options:folderSlug:)` returns the five `RenderedArtifact`s of the meeting
  folder (`meeting.json` first, because the crashed-attempt rule recognises a folder by that file
  alone) and `renderPersonPages(_:options:folderSlug:)` returns one `PersonPage` (`fileName`, `page`,
  `line`) per person. `renderPersonLine` is folded into `PersonPage.line`. Both take an optional
  `folderSlug`; the destination passes the pinned folder's basename on re-export so note names and
  the info-line links follow the folder, not a changed title. The transcript and tasks notes never
  mention the folder, so `renderTranscript` and `renderTasks` take no slug.
- `RenderedArtifact` has no `personID` and `Kind` has neither `.audio` nor `.personPage`: the
  destination places person pages from `PersonPage.fileName` and copies the audio itself.
- `RenderOptions.peopleFolder: String?` is `personPages: Bool`: no renderer ever read the string
  (the destination places the pages), and the flag no longer couples pages to `.wikilink`, so a
  plain-link destination can have pages too. Names are wikilinked only with `.wikilink` and pages.
- `ObsidianLayout` does not exist; `MeetingFolder` (Naming/) holds the whole layout, with
  `noteName(_:slug:)` as the extension-less wikilink target and `noteFile(_:slug:)` for the file.
- The delivery policy lives in `DeliveryLedger` (Runtime/, internal): which receipt applies to this
  root, the write rule, carry-over, receipt assembly and the collision rule as a static over two
  closures. The destination renders, asks the ledger and writes; a WebDAV destination reuses it.
  `DeliveryReceipt.folderURL` (adapters extension) is the `root + folder` join.
- `destinations(for:)` is `DeliveryCoordinator.destinations(for:)`, not a module-level function.
  `ManagedBlock` is internal until a second destination touches user-owned files.
- `ObsidianFolderDestination.init(settings:timeZone: = .current)` replaces `init(settings:
  fileManager:)`: `FileManager` is not `Sendable` in Swift 6, and the time zone is the one input the
  integration and end-to-end tests must pin. The sink stays the internal seam.
- `Slug.title` strips diacritics by canonical decomposition and dropping combining marks (Unicode
  data in the standard library) rather than `folding(options:locale:)`; same output, no locale API
  in the path at all.
- Under `## Summary` the folder note demotes core's section headings by one level (`###`), the
  offset `SummaryMarkdown` explicitly leaves to adapters; "verbatim" would have left `## Summary`
  an empty section followed by sibling `##` headings.
- The audio copy is named after the mixdown's extension (`audio.m4a` for the AAC mixdown,
  `audio.wav` for core's WAV decoder in the CLI and end-to-end tests) so the bytes and the name never
  disagree.
- A name is wikilinked only when it belongs to a `Person` (who has a page): participants without a
  person, cluster labels and free-text assignees stay plain. An assignee the model named by cluster
  label (`Speaker 1`) resolves through the speaker to its person, as `SummaryMarkdown` does for
  summary text. The plan's `[[assigneeName]] if any` would have produced dangling links such as
  `[[Speaker 1]]`.
- `DeliveredFile.relativePath` is relative to the receipt's `root` (the vault), not to the meeting
  folder, so person pages under `People/` sit in the same list as `Meetings/<folder>/meeting.json`.
- On re-export a freshly rendered path that is not in the previous receipt and already exists on
  disk is skipped and left out of the receipt ("files the app never wrote are never opened for
  writing"); a path that is absent is written. `ObsidianDestinationIntegrationTests.
  filesTheAppNeverWroteAreNotOpenedOnReexport` pins it.
- When `MeetingStore.export` fails, `DeliveryCoordinator.deliverAll` returns one `.failed("export
  failed: …")` row per configured destination (saved when the meeting row exists) instead of an
  empty list, so the failure reaches `observeDeliveries`. When `Settings` do not load, every stored
  delivery of the meeting becomes a `.failed("settings failed: …")` row with its receipt kept, for
  the same reason; only "no destination configured" is still an empty list.
- `steno deliver` gained `--vault`, `--people-folder`, `--include-audio` and `--task-tag`
  (`ObsidianOptions`, an `@OptionGroup`) for a one-off run that never touches the stored settings
  (the `process --audio-folder` precedent). The run uses its own destination id,
  `obsidian-folder@<standardised vault path>` (`ObsidianFolderDestination.init(settings:timeZone:id:)`),
  so it never replaces the stored destination's `Delivery` row and receipt, and a second run into the
  same vault is a proper re-export; the command prints only this run's destinations. Without
  `--vault` the stored `Settings.obsidian` decides and a missing one exits 2.
  `Wiring.dependencies(store:settings:dispatcher:)` gained the optional `dispatcher` and defaults to
  `DeliveryCoordinator`, so `steno process` delivers too when a vault is configured.
- The transcript and tasks notes carry `title: "<title> — Transcript"` / `"<title> — Tasks"` and
  start with an H1 of the same text; the plan only named the `title` key.
- The frontmatter `participants` list is the participants in export order, then speakers that
  resolved to nobody under their cluster label; names are deduplicated.
- The Ruby YAML round trip runs under `#if os(macOS)` (Linux containers have no `/usr/bin/ruby`);
  the byte assertions run everywhere.
- `Tests/StenoEndToEndTests` compares the folder note, transcript, tasks, VTT and both person pages
  with `snapshots/e2e/`; `meeting.json` is compared with a re-encode because it embeds the temp
  paths and the retention stage sets `expiresAt` after delivery, which also means the receipt's
  `meeting.json` hash legitimately changes on the redelivery the test performs.
- Step 9 (`[manual]` check in a real vault with Tasks, Dataview and Folder Notes) was not run in
  this PR; the unverified items in Research notes stay open for the app workstream's first vault.

Recorded 2026-09-25 while applying the correctness and elegance reviews of PR #7 (the "Review
application" comment on the PR maps every finding to accept, reject or follow-up):

- A receipt applies only to the root it was written for. `previous.root` and the current vault
  path are compared through `URL(fileURLWithPath:).standardizedFileURL.path` (trailing slash, `.`,
  `..` and a `/private` prefix are one root); a receipt from another root makes the delivery a first
  delivery, so the collision rule runs and every file is written. The plan's "folder pinned by the
  previous receipt" therefore holds only within one vault; a moved vault gets the scope's folder
  name again (`-2` if that folder now belongs to another meeting).
- With `includeAudio`, a missing mixdown (nil URL or file gone after the retention sweep) is no
  error when `audio` or `audio.*` is already in the meeting folder, listed in the receipt or on
  disk; `audioUnavailable` is thrown only when neither exists. A mixdown that exists but cannot be
  read is `readFailed(path:underlying:)`, a new `ObsidianError` case also used for an unreadable
  person page ("Could not read …"); `writeFailed` keeps the write verb.
- A person page that is not UTF-8 text is left untouched and the delivery fails with `readFailed`
  ("not UTF-8 text; the page was left unchanged") instead of the lossy decode that rewrote foreign
  bytes as U+FFFD. Operating on `Data` was the alternative; refusing keeps `ManagedBlock.merge` a
  pure `String -> String` and never alters a byte the user wrote.
- `peopleFolder` with surrounding whitespace is rejected by `validate` and `deliver` alike (it was
  trimmed in one and used verbatim in the other).
- `Frontmatter.quoted` escapes C1 controls (U+0080…U+009F), U+2028, U+2029 and U+FEFF as `\uXXXX`
  in addition to C0 and DEL.
- `ArtifactRenderer.version` is 2: the only byte change is the quoted tag lines in the folder note.
