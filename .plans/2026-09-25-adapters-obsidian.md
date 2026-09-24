# Steno adapters: destination runtime and the Obsidian folder destination

Status: implementation plan, 2026-09-25. Program: [`2026-09-25-v1-program.md`](2026-09-25-v1-program.md).
Scope authority: [`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md).
Owns `Sources/StenoAdapters` and `Tests/StenoAdaptersTests`. Requested program
changes are in the last section. Most important: the scope's task line
`- [ ] text 📅 due ⏫ [[Assignee]]` does not parse in the Obsidian Tasks plugin
(it reads backwards and stops at the first unrecognised token; only tags and
`^block-id` may follow the fields). Steno writes `- [ ] text [[Assignee]] ⏫ 📅 YYYY-MM-DD`.

## Goal

Turn a `.ready` meeting into files on disk, reliably and repeatably. Three
layers: pure renderers from the canonical model to bytes (folder note,
transcript, tasks, WebVTT, JSON, person page); a `DeliveryCoordinator` that
runs every enabled `Destination` sequentially with one `Delivery` row per
meeting and destination; and one concrete destination, the Obsidian vault
folder, laid out as the scope says. Re-export rewrites only files the app
wrote (tracked in a receipt) and never deletes anything. Renderers know
nothing about vaults, so a later WebDAV or Drive destination reuses them.

## Non-goals

- WebDAV, Google Drive, Nextcloud, webhooks, Notion, CRMs, MCP, REST (seam
  designed, not built). SRT, PDF, DOCX, HTML.
- Reading the vault back, two-way sync, watching for edits, git, `.obsidian/`.
- User-editable note templates (only path template, people folder, audio
  opt-in and task tag are configurable); retry scheduling or background
  queues (explicit re-export is the retry).
- Deleting or moving vault files for any reason, including audio retention
  (once `audio.m4a` is in the vault it belongs to the user); renaming the
  meeting folder when a summary re-run changes the title (pinned at first delivery).

## Research notes

Verified 2026-09-25 against Tasks user guide, Obsidian help, Dataview docs,
W3C WebVTT and YAML 1.2.2. Tasks: 🔺 highest, ⏫ high, 🔼 medium, none
normal, 🔽 low, ⏬ lowest; `📅 YYYY-MM-DD` due; fields follow the description
and only tags and block links may follow them; NBSP and U+FE0F break
recognition, so Steno emits bare code points. Obsidian properties: Date
`2020-08-21`, Date & time `2020-08-21T10:30:00`, Number, List, Tags (no
`#`); `tags`, `aliases`, `cssclasses` are defaults; wikilinks quoted.
Dataview: keys lowercased with spaces as dashes; ISO text is a date only
with `T`; durations are text like `1h 30m`. Folder notes: `Folder/Folder.md`
is the default of the xpgo and LostPaul plugins. WebVTT: `hh:mm:ss.ttt`;
`<v Name>text` may omit `</v>` as sole span; text escapes `&` `<`;
annotations may not contain `&` `>` newlines; `NOTE` blocks allowed; no
`-->` in payloads. YAML: plain scalars cannot start with an indicator or
contain `: ` or ` #` and are implicitly typed; double-quoted scalars escape
`\` `"` `\uXXXX`.

Unverified, checked in step 9: `yes`/`no` as booleans in Obsidian's parser
(moot, strings are quoted); Dataview parsing `"1h 30m"` as duration (moot,
integer minutes); quoted `"[[Name]]"` list entries as real links in graph
and backlinks; Tasks recognising `⏬` without U+FE0F.

## Public API (`Sources/StenoAdapters`)

```swift
// Rendering: pure, deterministic, no I/O.
public struct RenderOptions: Sendable, Equatable {
    public var linkStyle: LinkStyle          // .none plain names, .wikilink
    public var peopleFolder: String?         // "People"; nil disables person links and pages
    public var taskTag: String?              // "task" for vaults with a Tasks global filter
    public var timeZone: TimeZone
    public static let plain: RenderOptions   // .none, no people, no tag, UTC
}
public enum LinkStyle: Sendable { case none, wikilink }
public struct ArtifactRenderer: Sendable {
    public init()
    public func render(_ export: MeetingExport, options: RenderOptions) throws -> [RenderedArtifact]
    public func renderFolderNote(_ export: MeetingExport, options: RenderOptions) -> String
    public func renderTranscript(_ export: MeetingExport, options: RenderOptions) -> String
    public func renderTasks(_ export: MeetingExport, options: RenderOptions) -> String
    public func renderVTT(_ export: MeetingExport) -> String
    public func renderJSON(_ export: MeetingExport) throws -> Data
    public func renderPersonLine(_ export: MeetingExport, folderSlug: String) -> String
}
public struct Frontmatter: Sendable, Equatable {
    public enum Value: Sendable, Equatable { case string(String), int(Int), bool(Bool), date(Date), dateTime(Date), list([String]) }
    public var fields: [(key: String, value: Value)]   // insertion order kept
    public func encoded() -> String                    // "---\n…---\n"; every string double-quoted
}
public enum Slug {
    public static func title(_ s: String, maxLength: Int = 60) -> String   // "produktstrategie-90-10"
    public static func fileName(_ s: String) -> String                     // strips / \ : * ? " < > | # ^ [ ] and controls
}
public struct PathTemplate: Sendable, Equatable {
    public static let `default` = PathTemplate("Meetings/{{date}}-{{slug}}")
    public init(_ raw: String)
    public func validate() throws                       // known variables, relative, no "..", no empty segment
    public func resolve(for meeting: Meeting, timeZone: TimeZone) -> String
}
public enum Timecode { public static func clock(_ t: TimeInterval) -> String; public static func vtt(_ t: TimeInterval) -> String }  // "00:12:34", "00:12:34.567"

// Runtime. DeliveryDispatcher is a StenoCore protocol, see last section.
public actor DestinationRegistry { public init(destinations: [any Destination]); public func destination(id: String) -> (any Destination)? }
public actor DeliveryCoordinator: DeliveryDispatcher {
    public init(store: MeetingStore, settings: SettingsStore, registry: DestinationRegistry,
                clock: @Sendable () -> Date = Date.init)
    public func deliverAll(meetingID: UUID, mode: DeliveryMode) async -> [Delivery]     // never throws
    public func reexport(meetingID: UUID, destinationID: String?) async -> [Delivery]   // nil = all enabled
}

// Obsidian.
public struct ObsidianSettings: Sendable, Equatable, Codable {
    public var vaultPath: String                 // absolute, must exist and be writable
    public var pathTemplate: PathTemplate        // default Meetings/{{date}}-{{slug}}
    public var peopleFolder: String?             // default nil
    public var includeAudio: Bool                // default false
    public var taskTag: String?                  // default nil
    public init(settings: DestinationSettings) throws
    public func asDestinationSettings() -> DestinationSettings
}
public struct ObsidianFolderDestination: Destination {
    public static let id = "obsidian-folder"
    public init(fileManager: FileManager = .default)
    public func validate(settings: DestinationSettings) async throws
    public func deliver(_ export: MeetingExport, settings: DestinationSettings, mode: DeliveryMode) async throws -> DeliveryReceipt
}
public enum ObsidianError: Error, Sendable, Equatable { case vaultMissing(String), vaultNotWritable(String), templateInvalid(String), writeFailed(path: String, underlying: String), audioUnavailable }

// Proposed for StenoCore (requested changes 3 and 4).
public struct DeliveryReceipt: Codable, Sendable, Equatable {
    public struct File: Codable, Sendable, Equatable {
        public enum Ownership: String, Codable, Sendable { case owned, managedBlock }   // whole file vs marked region
        public var relativePath: String, ownership: Ownership, sha256: String
    }
    public var root: String               // vault path at delivery time
    public var folder: String             // "Meetings/2026-09-24-produktstrategie-90-10", pinned
    public var files: [File]
    public var rendererVersion: Int       // bumped when output format changes
}
public struct RenderedArtifact: Sendable, Equatable {
    public enum Kind: String, Sendable { case folderNote, transcript, tasks, vtt, json, personPage, audio }
    public var kind: Kind, fileName: String, data: Data, personID: UUID?   // fileName is a basename
}
```

## Default folder layout and formats

Slug. `Slug.title` lowercases, transliterates ä→ae ö→oe ü→ue ß→ss, strips
other diacritics (`.diacriticInsensitive`), replaces runs outside `[a-z0-9]`
with one hyphen, trims hyphens, cuts to 60 characters at the last hyphen,
falls back to `meeting`. Folder: `pathTemplate.resolve`, default
`Meetings/{{date}}-{{slug}}`; variables from `startedAt` in the user's zone:
`{{year}}` `2026`, `{{month}}` `09`, `{{day}}` `24`, `{{date}}` `2026-09-24`,
`{{slug}}`, `{{title}}` (`Slug.fileName(title)`). The folder basename is the
slug used in file names. Collision on `.initial`: a folder whose
`meeting.json` has another `steno_id` (or none) gets `-2`, `-3`, … appended;
one with our `steno_id` is a crashed attempt and is reused.

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

Frontmatter (folder note) comes from `Frontmatter.encoded()`, never from
interpolation; every string is double-quoted with `\\`, `\"` and controls
escaped, so colons, quotes, `#` and leading `-` in titles are safe:

```yaml
---
title: "Produktstrategie: \"90/10\" & Roadmap"
date: 2026-09-24T14:00:00      # Obsidian Date & time, Dataview date; local time, no offset
duration: 90                   # whole minutes, rounded up; Obsidian Number
participants:
  - "[[Anna Müller]]"          # plain "Anna Müller" when peopleFolder is nil; "Speaker 2" unlinked
tags:
  - meeting                    # always present
  - kunde/acme                 # user tags: no #, spaces to -, only [A-Za-z0-9_/-]
source: "mac-call"             # mac-call | mac-in-person | phone
template: "default"
language: "de"                 # Locale.Language.minimalIdentifier
steno_id: "0d6f…"              # lowercase UUID
---
```

`title` is added to the scope's list because the filename is a slug. Body:
`# Title`; info line `2026-09-24 14:00–15:30 · 1 h 30 min · Mac call ·
[[<slug> - Transcript|Transcript]] · [[<slug> - Tasks|Tasks]]`; `## Summary`
(LLM Markdown verbatim, headings at level 2+); `## Decisions` (bullet per
`Decision.text`) and `## Scratchpad` (verbatim), each omitted when empty.

Transcript note. Frontmatter `title`, `steno_id`, `type: "transcript"`. One
`## Anna Müller — 00:12:34` header per turn (consecutive segments by one
speaker; name wikilinked with people pages on; `Timecode.clock` of the first
segment), blank line inside a turn at gaps of 3 s or more; a paragraph
starting with `#`, `-`, `>` or `1.` is backslash-escaped.

Tasks note. Frontmatter `title`, `steno_id`, `type: "tasks"`. One line per
`MeetingTask`, fixed order: text, `[[assigneeName]]` if any, `#taskTag` if
set, priority emoji, due date:

```
- [ ] Angebot an ACME schicken [[Anna Müller]] #task ⏫ 📅 2026-10-01
- [x] Protokoll verteilen 🔽
```

Priority from `MeetingTask.priority`: `.high` → `⏫`, `.normal` → none,
`.low` → `🔽`; `🔺 🔼 ⏬` unused with three levels (highest/lowest if the
model grows). `done` writes `- [x]` without `✅` (no completion date in the
model). Newlines become spaces. Closing line: "Edit tasks in Steno; this file
is rewritten on re-export."

WebVTT. `WEBVTT - Steno <steno_id>`, a `NOTE` block with title and date, one
cue per `TranscriptSegment` in start order, `Timecode.vtt(start) -->
Timecode.vtt(end)` (end at least start + 1 ms), payload `<v Anna Müller>text`.
Text escapes `&` `<` `>`, drops CR LF `-->`; annotation drops `&` `>` CR LF.

JSON. `schema_version: 1`, `meeting`, `participants`, `speakers` (no
embeddings), `segments` (`lane`, `text`, `raw_text`), `tasks`, `decisions`,
`audio` (`{"file": "audio.m4a"}` or `null`), `delivery` (`renderer_version`,
`exported_at`). `JSONEncoder` `.sortedKeys` `.prettyPrinted`
`.withoutEscapingSlashes`, snake_case, ISO 8601 with fractional seconds and
offset. The machine-readable truth for the CLI and agents.

Person page `People/<Slug.fileName(displayName)>.md`, display name as
filename so `[[Anna Müller]]` resolves. Created when missing with frontmatter
(`steno_person_id`, `email?`, `type: "person"`) and `# Name`. The app owns
only the region between `<!-- steno:meetings:start -->` and `<!-- steno:meetings:end -->`
(appended when missing): one line per meeting, newest first,
`- 2026-09-24 [[<folder slug>|<title>]] %%steno:<meeting uuid>%%`; the line for
this uuid is replaced or inserted, bytes outside the markers copied
unchanged. Renamed people get a new page; the old one stays.

Audio. Copied byte for byte when `includeAudio` is on and the `AudioAsset` is
an AAC `.m4a` mixdown; otherwise `.audioUnavailable` after all other files are written.

## Delivery runtime

`deliverAll` loads the bundle from `MeetingStore`, builds `MeetingExport`
with `artifacts` rendered under `RenderOptions.plain`, reads
`enabledDestinationIDs` and per-destination settings from `SettingsStore`,
then per destination in settings order: upsert the `Delivery` row to
`.pending` with `lastAttemptAt = now`; call `deliver` with `.initial` or
`.reexport(previous:)`; store `.delivered(now)` plus receipt, or
`.failed(String(describing: error), now)`; move on. Strictly sequential; one
failure never blocks the next. `reexport` takes the same path; a row without
receipt (never delivered, or folder removed by the user) falls back to `.initial`.

Obsidian `deliver`: resolve `ObsidianSettings`; pick the folder (receipt on
re-export, template plus collision rule on initial); render with
`RenderOptions(linkStyle: .wikilink, peopleFolder:, taskTag:)`; remove stale
`.steno-tmp-*`; write each artefact through `AtomicFileWriter` (temp
`.steno-tmp-<name>-<8 hex>` in the target folder, `fsync`, `rename(2)`);
merge person blocks via `ManagedBlock`; return the receipt. On re-export only
paths in the previous receipt or freshly rendered are written; a file no
longer produced (audio opted out, people off) stays on disk and in the
receipt. Files the app never wrote are never opened for writing; nothing is
deleted except the app's own temp files. `validate`: vault path is a
writable directory (probe `.steno-probe-<hex>` created and removed);
template validates; people folder relative without `..`; missing
`.obsidian/` is a warning, not an error.

Future WebDAV or Drive destination: conforms to `Destination`, calls
`ArtifactRenderer.render(export, options:)` with its own `RenderOptions`
(likely `linkStyle: .none`), resolves the folder with the same
`PathTemplate`, pushes each `RenderedArtifact.data` to `folder/fileName`
through its transport, and returns a `DeliveryReceipt` whose `root` is the
remote base URL. The Obsidian destination already writes through a small
non-public `FileSink` with one `LocalFolderSink`; a `WebDAVSink` would be
the second implementation, at which point `FileSink` goes public per the
two-implementations rule. Renderers, slugs, templates, receipts: unchanged.

## Files

```
Sources/StenoAdapters/
  Rendering/ArtifactRenderer.swift, RenderOptions.swift      entry point; options and LinkStyle
  Rendering/Frontmatter.swift, MarkdownEscaping.swift        YAML emitter (no dependency); escaping, wikilinks, tag sanitiser
  Rendering/FolderNoteRenderer.swift, TranscriptMarkdownRenderer.swift, TasksMarkdownRenderer.swift
  Rendering/WebVTTRenderer.swift, MeetingJSONRenderer.swift, PersonPageRenderer.swift, Timecode.swift
  Naming/Slug.swift, PathTemplate.swift                      slug table and sanitiser; parse, validate, resolve
  Runtime/DestinationRegistry.swift, DeliveryCoordinator.swift, ExportCommand.swift   `steno export` wiring
  Obsidian/ObsidianSettings.swift, ObsidianFolderDestination.swift, ObsidianLayout.swift, ManagedBlock.swift
  FileSystem/FileSink.swift, AtomicFileWriter.swift          internal sink protocol; temp + fsync + rename
Tests/StenoAdaptersTests/
  Support/FixtureMeeting.swift, Snapshot.swift               fixture loader; golden compare, STENO_UPDATE_SNAPSHOTS=1
  one <Type>Tests.swift per public type above, plus ManagedBlockTests, AtomicFileWriterTests,
  ObsidianDestinationIntegrationTests, DeliveryCoordinatorTests
Tests/Fixtures/meetings/produktstrategie.json                synthetic meeting (step 1)
Tests/Fixtures/snapshots/obsidian/*                          one golden file per renderer output and variant
Package.swift                                                adds StenoAdapters (depends on StenoCore only) and its test target; no third-party packages
```

## Steps

Each step is at most one day; checks run as `swift test --filter StenoAdaptersTests.<Name>` on the `macos-15` CI job.

1. Target, fixture, naming. Fixture: title `Produktstrategie: "90/10" &
   Roadmap für Q4`; three participants, one unknown `Speaker 2`; 14 segments
   over both lanes, one containing `<`, `&`, a leading `# ` and `-->`; tasks
   covering all priorities, with and without due date and assignee, one
   done; two decisions; scratchpad containing `---` and `## Summary`; tags
   `Kunde ACME`, `#q4`. Implement `Slug`, `PathTemplate`, `Timecode`.
   Check: `SlugTests` (umlauts, ß, é, emoji-only → `meeting`, 61-char cut,
   forbidden characters), `PathTemplateTests` (six variables, `..` and
   unknown variable rejected), `TimecodeTests` pass.
2. Frontmatter emitter, Markdown escaping, tag sanitiser.
   Check: `FrontmatterTests` golden plus cases for `: `, `"`, `\`, leading
   `-`, ` #`, `yes`, `2026-09-24`, empty string, control character; opt-in
   round trip when `ruby` is on PATH via
   `ruby -ryaml -rjson -e 'puts YAML.safe_load(STDIN.read, permitted_classes: [Date, Time]).to_json'`.
3. Folder note, transcript, tasks renderers, `.plain` and `.wikilink`.
   Check: six golden files match; tasks test asserts by regex that each line
   ends with the emoji fields, has no U+FE0F or U+00A0, `.normal` no emoji.
4. WebVTT and JSON renderers.
   Check: golden files match; VTT test asserts header, ordering, `end >
   start` for a zero-length segment, escaping of the `<`/`&`/`-->` segment;
   JSON test round-trips through a test DTO and asserts no `embedding` key.
5. `AtomicFileWriter`, `LocalFolderSink`, `ManagedBlock`, receipt SHA-256.
   Check: no `.steno-tmp-*` left, content matches; read-only target yields
   `.writeFailed` without residue; `ManagedBlockTests` cover markers appended
   when missing, bytes outside preserved, line replaced by uuid, newest first.
6. `ObsidianFolderDestination` initial delivery and `validate`.
   Check: delivery into a temp vault yields exactly the six-file layout, each
   file equal to its golden, receipt with five owned files (six with audio)
   plus two managed blocks; `validate` rejects a missing path and an
   unwritable directory with typed errors, accepts an empty directory.
7. Re-export, collision, preservation.
   Check: after the test adds `notes.md`, edits the person page around the
   block and changes the title, `.reexport` leaves `notes.md` and the edits
   intact, keeps the folder name, updates `title`, keeps `audio.m4a` after
   `includeAudio` is off; a second meeting resolving to the same folder gets
   `-2`; a rerun `.initial` for the same `steno_id` reuses it.
8. `DestinationRegistry`, `DeliveryCoordinator`, `ExportCommand`.
   Check: two fake destinations, first throwing → `.failed` with error text
   and `.delivered` with receipt, `lastAttemptAt` on both; `reexport` passes
   the stored receipt back; disabled destination gets no row;
   `steno export --meeting <id> --destination obsidian-folder` runs against
   the core fixture database.
9. Manual check in a real vault with Tasks, Dataview and Folder Notes: folder
   note opens on folder click; properties show `date` as Date & time,
   `duration` as Number, `participants` as links; a Tasks `not done` query
   lists the fixture tasks with due dates and priorities; Dataview `FROM
   #meeting` shows the note; `[[Anna Müller]]` resolves; `transcript.vtt`
   plays in VLC. Record results for the unverified items.

## Tests

- Unit (steps 1, 2, 5, 8; no network, no real audio) and golden snapshots
  under `Tests/Fixtures/snapshots/obsidian/`: folder note plain/wikilink,
  transcript plain/wikilink, tasks plain/wikilink/with tag, VTT, JSON, new
  person page, merged block. Failure prints a unified diff;
  `STENO_UPDATE_SNAPSHOTS=1 swift test` rewrites, reviewed in the PR.
- Filesystem integration in a fresh temp directory per test: initial
  delivery, re-export, user-file preservation, collision, crash reuse, audio
  opt-in with a 100-byte fake `.m4a`. Coordinator integration: in-memory
  GRDB store from core plus fake destinations. Manual: step 9.

## Spikes

- S1 (before step 3, half a day): in a real vault, Tasks recognises the
  emitted line order with the wikilink before the emojis and `🔽`/`⏫`
  without U+FE0F. Failure changes the line order, not the architecture.
- S2 (before step 2): `ruby -ryaml` exists on the `macos-15` runner; otherwise the round trip stays skipped and step 9 carries it.
- S3 (before step 6): the audio workstream delivers one AAC `.m4a` mixdown per meeting; otherwise `.audioUnavailable` as written.

## Needs from other workstreams

- Core foundation: `MeetingExport` as canonical bundle (`Meeting`,
  `[Participant]`, `[Person]`, `[Speaker]`, `[TranscriptSegment]`,
  `[MeetingTask]`, `[Decision]`, `AudioAsset?`) plus `artifacts`;
  `DestinationSettings` typed getters; `DeliveryMode` with previous receipt;
  `Delivery` receipt column; `MeetingStore` bundle read and `Delivery`
  upsert/query; `SettingsStore` `enabledDestinationIDs` and
  `destinationSettings(id:)`; `DeliveryDispatcher` in pipeline step 9; CLI
  hook for `ExportCommand`; stable `templateID` strings.
- LLM (`LanguageModel` output): summary headings at level 2 or deeper, no
  frontmatter or leading `---`; task and decision text single-line.
- Speech (`Diarizer`, `SpeakerMemory`): stable `clusterLabel` so `Speaker N`
  numbering is deterministic across re-exports.
- Audio: `AudioAsset.format` distinguishes the AAC `.m4a` mixdown (S3).
- macOS app: settings UI (vault picker, path template with live preview,
  people folder, audio opt-in, task tag); delivery status and re-export
  button from `Delivery` rows; `ObsidianError` messages shown verbatim.

## Requested changes to the program document

1. `Destination.deliver` returns `DeliveryReceipt` (`… async throws ->
   DeliveryReceipt`); without it re-export cannot know what the app wrote.
2. `DeliveryMode` becomes `case initial`, `case reexport(previous: DeliveryReceipt?)`.
3. `Delivery` gains `receipt: DeliveryReceipt?` stored as JSON text;
   `DeliveryReceipt` lives in StenoCore with the shape above.
4. `MeetingExport` is the canonical bundle plus `artifacts: [RenderedArtifact]`,
   constructed by `DeliveryCoordinator` in StenoAdapters; `RenderedArtifact`
   moves to StenoCore.
5. StenoCore defines `public protocol DeliveryDispatcher: Sendable { func deliverAll(meetingID: UUID, mode: DeliveryMode) async -> [Delivery] }`;
   pipeline step 9 calls it; app and CLI inject `DeliveryCoordinator`.
6. Scope corrections: task line `- [ ] text [[Assignee]] ⏫ 📅 YYYY-MM-DD`;
   frontmatter gains `title`; `duration` is whole minutes as a number.
   Note for the LLM workstream: summary headings start at level 2.
