# Steno first-run feedback plans: elegance review

Date: 2026-09-28. Reviewer: architecture, design system and naming pass over the
seven plans indexed by `2026-09-28-first-run-feedback.md`, as committed at `9cd7cf5`
("docs(plans): simplify the first-run feedback plans"). Binding context read:
`2026-09-24-initial-scope.md`, `2026-09-25-macos-app-and-release.md` ("the app target
contains AppKit/SwiftUI glue and view models only"), `2026-09-25-v1-program.md`, and the
design-craft skill (values, recipes, accents) for the visual vocabulary the redesign
adopts. Owner decisions listed as fixed in the review brief are not re-opened; where a
finding touches one, it is about the mechanism or the affordance, not the decision.
Nothing below edits a plan.

**major**: expensive once code compiles against it, or two plans that will ship
visuals or copy that contradict each other. **minor**: one PR. **nit**: one line.

## Findings

1. **major**: `macos-visual-redesign.md` decision 3 and `RecordingControl` row;
   `start-recording-from-main-window.md` "Stop is a secondary button carrying
   destructive colour"; `floating-recording-indicator.md` decision 6; index "Shared
   decisions". One state, recording, is painted with two hues and the second hue
   changes by surface. In the window and the menu bar the recording control shows a
   `destructive` dot and label next to `live-bright` level bars, inside one 40 pt
   control. The floating plan then argues, correctly, that "green `live` bars next to
   a red square would spend two hues inside 40 pt" and makes the bubble's bars
   `strong`. So the same meter is green in the sidebar, green in the detail header
   (240 pt `LevelBars`, fills `live-bright`), green in the menu bar and achromatic in
   the bubble that floats over all three. Design-craft spends hue by role: one
   semantic colour per state, containment for emphasis. Fix, across the four plans:
   recording is red. The dot, the bubble's stop control and the menu bar symbol carry
   `destructive`; every level meter (`LevelBars`, the bubble's five bars) is
   achromatic (`strong` for the fill, `border` for the track) on every surface; `live`
   and `live-bright` are reserved for success (granted permission glyph, delivered
   chip). The redesign's decision 3 sentence "recording state is shown by the
   `destructive` dot and the `live` level bars" becomes "by the `destructive` dot; the
   meters are achromatic". One line in each plan, no new token.

2. **major**: `macos-visual-redesign.md` "Recording and processing states", row "Ready,
   no content in a tab", and `PendingText` "picks the row of this table from
   `Meeting.state`"; `onboarding-vault-and-llm.md` "Meeting detail" (`summaryStatus`
   with `skippedUnconfigured` and `skippedRunnable`, "`TabText` is left alone").
   Two plans specify the Summary and Tasks tab bodies for a `.ready` meeting with
   `summary == nil`. The onboarding plan (lands at step 4) renders "Summary skipped: no
   LLM endpoint is configured. The transcript is complete." with a "Set up summaries"
   button, selected by `summaryStatus`. The redesign (lands last) renders an
   `EmptyState` titled "No summary" with body "The template produced no sections.",
   selected by `Meeting.state`, and says `PendingText` keys off state alone. After the
   redesign the honest copy is gone and the pane states a cause that is false for
   every unconfigured install. The same split exists in the footer: the onboarding
   plan writes "Not exported: no Obsidian vault is configured." / "Not exported yet." /
   "Export now", the redesign writes "Not delivered yet" and "Delivered 10:02". Fix:
   the redesign's states table gains the onboarding plan's rows verbatim
   (`skippedUnconfigured`, `skippedRunnable`, `noVault`, `notExported`) and names
   `MeetingDetailViewModel.summaryStatus` and `exportStatus` as the selectors for tab
   bodies and footer; `EmptyState` gets an optional action so the button survives.
   One vocabulary in user copy: "export" (the Actions menu already says "Re-export"),
   never "delivered".

3. **major**: `macos-visual-redesign.md` decisions 4 and 7, "Nav column". The only
   Start recording control in the window lives in the nav column, and "the nav column
   collapses with the standard toggle". A user who collapses it (or opens the window
   under 960 pt after the split view collapses it for them) has a main window with no
   way to record, which is the exact complaint that started this round. Fix, one of:
   fix `columnVisibility` to `.all` and remove the toggle from the toolbar, or when
   the nav column is collapsed place a compact `RecordingControl` (32 pt, primary) as
   the first toolbar item of the list column. The first is smaller and matches Jamie,
   whose nav never collapses.

4. **major**: `macos-visual-redesign.md` decision 13 and step 3
   (`Meeting.displayTitle(now:calendar:timeZone:)` in `apps/macos/Steno/Design/Labels.swift`,
   test "a default title in another time zone is still recognised"). The app decides
   whether a stored title is the machine default by re-running
   `LocalRecordingIntake.defaultTitle(source:startedAt:)` and comparing strings. Core
   knows this fact at write time (`begin` chooses the default at
   `LocalRecordingIntake.swift:105`, the calendar overwrites at stop, `Summarize`
   replaces it after processing); the app rediscovers it by formatting, which breaks
   when the recording was made in another time zone, when the format string changes,
   or when a user one day types exactly that string. The CLI list output and the iOS
   app would need the same fact and cannot reach `Labels.swift`. The device-change
   plan opens migration `v3` in the same round. Fix: `Meeting.titleOrigin:
   TitleOrigin` (`.default`, `.calendar`, `.summary`, `.user`) set by core in the three
   places that write `title`, one column in the same `v3` migration, nil-omitting
   Codable so fixtures stay. `displayTitle` then reads `titleOrigin == .default` and
   only formats. The redesign's "no StenoCore changes" non-goal bends by one column;
   the alternative is a formatter round trip that the plan's own test list already
   shows is fragile.

5. **major**: `first-run-feedback.md` "Order", step 8 versus steps 3, 4 and 6. The
   redesign rewrites `Card`, `StatusChip`, the button styles, `MessageRow`,
   `readingColumn()`, and adds `EmptyState`, `StenoTextFieldStyle`, `IconButton`,
   `SegmentedTabs`. Retention (step 3, detail "Recording" line and the Audio tab),
   onboarding (step 4, page 2 rows, banner, footer states) and device change (step 6,
   detail `MessageRow`, list meta) each build UI on the current components, and the
   redesign then restyles every one of those screens. Fix: split the redesign plan's
   steps 1 and 2 (tokens, components, previews; no layout, no behaviour) out as its
   own PR that lands directly after step 1 of the index. Steps 3 to 6 then compose from
   the final components and the redesign's layout steps (3 to 13) stay last. Detail
   in Sequencing below.

6. **minor**: `macos-visual-redesign.md` `MeetingEntry` ("time `h:mm` 12 `muted`") and
   decision 13 (`HH:mm`). `h:mm` is a 12-hour pattern with no AM/PM marker: a German
   user on a 24-hour clock sees "1:06" for a 13:06 meeting, and the same plan uses
   `HH:mm` two sections later. Use `Date.FormatStyle` (`.hour(.defaultDigits(amPM:
   .abbreviated)).minute()`) and `.weekday(.wide)` / `.month(.abbreviated).day()`
   everywhere the plan writes a literal pattern, so 12 versus 24 hour, "Sep 28"
   versus "28. Sept." and weekday names follow the system locale. Applies to the
   bubble's and menu bar's elapsed clock (fine, that is a duration) only in the sense
   that `clockText` stays; every wall-clock time goes through `FormatStyle`.

7. **minor**: `macos-visual-redesign.md` "Detail pane" header; `audio-retention-keep-forever.md`
   "Detail header, Recording line" ("under the meta line and above the tags row");
   `device-change-during-recording.md` "Copy" (`MessageRow(kind: .info)` "under the
   meta line"); `onboarding-vault-and-llm.md` "Main window banner" ("top of the detail
   column"). Four plans insert rows under the same meta line and none owns the stack.
   The redesign's per-screen layout does not mention `SetupBanner`, the end-reason
   row or the retention line at all, so three surfaces ship unstyled by the plan that
   restyles everything. Fix: the redesign's header spec lists the stack once: banner
   (above the header, full width, dismissable), title row, meta row, end-reason
   `MessageRow` (device plan), retention line (retention plan), progress or level
   bars, tags row, tabs. Each sibling plan then says "row N of the redesign's header
   stack".

8. **minor**: `audio-retention-keep-forever.md` decision 6 and "Detail header,
   Recording line". With the default now "Forever", every meeting's header carries
   "Recording kept forever", a line that says what Settings already says and adds a
   Finder button to the eye line. Show the line only when it says something the
   default does not: "Deletes on 28 Oct 2026", "Kept until the export succeeds",
   "Kept; processing failed", "Recording deleted". For `.keepForever` with files
   present render nothing; "Reveal recording in Finder" stays in the Actions menu.
   The copy "Recording deleted on <future date>" reads as past tense; "Deletes on".

9. **minor**: `device-change-during-recording.md` step 8 (`AutoStopCountdown`,
   "the `DetectionPromptViewModel` pattern, kept separate"); `floating-recording-indicator.md`
   step 2 (`fractionRemaining` on `DetectionPromptViewModel`). Two `@MainActor
   @Observable` classes, each with `remaining`, `fractionRemaining`, a per-second tick
   on the injected clock and an elapsed callback, driving the same 2 pt hairline. Same
   idea under two names. One `Countdown(duration:clock:onElapsed:)` in
   `Recording/Countdown.swift` with `remaining`, `fractionRemaining`, `remainingText`;
   `DetectionPromptViewModel` holds one, `RecordingController.autoStop` is
   `(appName: String?, countdown: Countdown)?`. One `CountdownHairline(countdown:)`
   view in the bubble file serves the prompt and the armed bubble.

10. **minor**: `device-change-during-recording.md` step 9 ("`RecordingControlPresentation`
    gains `autoStopLine` and `showsKeepRecording`"; `BubblePresentation.make(state:autoStop:)`
    gains the row; menu bar renders `recorder.autoStop`). The countdown sentence is
    composed in three places. Per-surface presentation structs are the right shape
    (the bubble has no permission state, the sidebar has no anchor), but the strings
    must have one owner: `AutoStop.line` ("<App> closed the microphone. Stopping in
    1:29.") on the recorder's value, and the presentations carry `autoStop:
    AutoStop?`, not a copied `String`. Same rule the plans already apply to
    `RecordingState.label` and `PermissionKind.deniedMessage`.

11. **minor**: `device-change-during-recording.md` "Surfaces while the auto-stop is
    armed". The owner asked for a suggestion; the plan reads it as a countdown that
    stops by default with a cancel. That reading is right for the common case (the
    call ended, the user walked away) and the decision is fixed. The affordance is
    not: "Keep recording" is "a text button before the stop button" in a 40 pt pill,
    the line is `xxs` `mutedForeground`, and the bubble's height for a second line is
    not specified (the box is height 40). Make the armed bubble two rows tall (64),
    the line `sm` `strong` (it is the one sentence to read), "Keep recording" a
    `StenoSecondaryButtonStyle` at 28 pt, and the red stop control unchanged, so the
    two choices have equal weight. In the sidebar and menu bar the `MessageRow(kind:
    .warning)` with a button is fine.

12. **minor**: `floating-recording-indicator.md` decision 6 versus
    `start-recording-from-main-window.md` "Stop is a secondary button carrying
    destructive colour" and the redesign's header Stop. The bubble's Stop is a filled
    `destructive` square with a white glyph; the window's Stop, 200 pt away, is a
    `raised` surface with a red dot and a red word. Two constructions for one action
    visible at once. The floating plan's reason ("an icon-only control has no label to
    carry the meaning") is answered without a red fill: a 28 pt `raised` hairline
    square with a `destructive` `stop.fill` glyph, hover `card`. Red stays a glyph or
    dot everywhere, never a fill, and the bubble reads as the same family as the
    control it mirrors.

13. **minor**: `floating-recording-indicator.md` decision 7 ("the countdown is a
    hairline, not a number ... the number invites reading and looks like a bug")
    versus `device-change-during-recording.md` bubble row ("Stopping in 1:29" plus
    the hairline). The device plan is right that its countdown needs a number (the
    user must decide); the prompt's is right that its own does not (it only fades).
    Both are fine, but the floating plan's reasoning is stated as a rule and the
    device plan breaks it two weeks later. Reword decision 7 as "the prompt shows no
    number because nothing is at stake when it closes; a countdown that ends a
    recording shows both" so the two are one rule.

14. **minor**: `macos-visual-redesign.md` "Colour tokens" (`sidebar` `#f4f4f5` /
    `#0a0a0a`, `raised` `#ffffff` / `rgba(255,255,255,0.031)`). `raised` as the one
    documented light-mode exception to alpha veils is the right correction of F1 and
    is well argued. `sidebar` is a second hand-picked opaque grey, which is the thing
    the ladder forbids; it is within a rounding of `background` plus the `card` veil
    in both appearances (`#fafafa` at 3.1 % black is `#f5f5f5`; black at 3.1 % white
    is `#080808`). Define it as that composite (`Theme.macTokens.sidebar =
    background.over(card)`) so light and dark stay derivable and the plan's own test
    "every surface is `raised`, a veil, or a hairline" holds without an exception list.

15. **minor**: `macos-visual-redesign.md` "Colour tokens" and "Type scale" (`faint`
    for "previews, meta rows, placeholders, notes"; preview 13 `faint`; times and meta
    12 `faint`). `faint` is the ladder's minimum normal-text tier (5.26:1 light,
    4.43:1 dark) and design-craft says "do not use for tiny essential text". The
    preview line is the content of the card and the meta row carries the duration;
    at 12 and 13 pt in dark mode they sit under 4.5:1. Move previews and meta to
    `muted-foreground`, keep `faint` for placeholders, footnotes and the step
    caption, and `ghost` for glyphs only. Four tiers on a screen, as the plan's own
    verification asks.

16. **minor**: `macos-visual-redesign.md` "Type scale" (`sm` 14/20 with
    `lineSpacing(6)`, `xs` 13/18) versus `apps/macos/Steno/Design/Theme.swift`
    `TextSize.sm = (14, 19)`, `xs = (13, 17)` mirrored from `mobile/global.css`, and
    F8 ("`lineHeight` is carried but never applied"). Fixing F8 by applying the stored
    leading gives 14/19, not the 14/20 the plan specifies. Say which: change
    `TextSize` (and the CSS it mirrors, or accept a Mac-only override in `macTokens`)
    to 20 and 18, or spec 14/19 and 13/17. Either is fine; two numbers for one token
    is not.

17. **minor**: `macos-visual-redesign.md` `RecordingControl` row ("the dot pulses with
    `Motion.pulse`"), "Recording" list entry row ("6 pt dot pulsing with
    `Motion.pulse`"), header Stop (dot, pulse unspecified), plus the bubble's live
    bars. With the live meeting selected, the window shows two or three pulsing red
    dots at once and the bubble moves beside them. Design-craft's budget is one
    ambient animation per viewport. Pulse only the dot that has no clock next to it
    (the list entry, which shows the elapsed time as text, or none); the sidebar Stop
    and header Stop have a ticking `mm:ss` and need no pulse. State the rule once
    under Motion.

18. **minor**: `macos-visual-redesign.md` decision 13 ("Call, Monday 10:06") and
    "Detail pane" meta row ("full date, time, duration, source label ..."). F31
    complained that the timestamp appears three times; the fix turns the heading
    into a friendlier date and time and keeps the meta row's date, time and source
    under it, so it still appears twice and the source word three times. When the
    title is derived, the meta row drops date, time and source and keeps duration,
    language and tokens. The comma form is also odd in both audiences' languages;
    "Monday 10:06 call" or the source as a neutral chip beside a "Monday 10:06" title
    reads better and localises.

19. **minor**: `onboarding-vault-and-llm.md` step 4 (`SetupState`, `@MainActor
    @Observable final class` owned by `AppEnvironment`, fed by an observer task over
    `settings.observe()`). Two booleans that are pure functions of `Settings`
    (`LLMEndpoint(settings:) != nil`, `settings.obsidian != nil`) mirrored into a
    class through a second observation, plus one real piece of state
    (`bannerDismissed`). Replace with `extension Settings { var llmConfigured: Bool;
    var vaultConfigured: Bool }` (in the app, or in `StenoLLM`/`StenoCore` so `steno
    process` prints the same reason from the same test) and `AppController.setupBannerDismissed:
    Bool`. The view models already observe `Settings`. Keep `derive` only as the
    test of the two properties.

20. **minor**: `device-change-during-recording.md` decision 7 (`CaptureNotice.deviceLost`)
    beside `CaptureState.failed(.deviceLost, recording:)`. The notice repeats a fact
    the state stream already carries, which is the two-channels-for-one-fact pattern
    the 2026-09-25 review removed from `MeetingEvent` (finding 14 there). Keep
    `.deviceChanged(reason)` and `.deviceResumed(attempt:gapSeconds:)`, which are not
    states; drop `.deviceLost` from the stream. The recorder's `.failed` observer
    already sets the error.

21. **minor**: `audio-retention-keep-forever.md` decision 4 and step 3
    (`AudioSettingsViewModel.setRetention` collects "assets whose master file exists"
    and calls `MeetingStore.clearExpiry(assetIDs:)`). The rule "switching to Forever
    un-stamps every recording still on disk" is retention policy with a file check,
    and `RetentionSweep` in core already owns exactly that pairing (rows plus files).
    Put it there as `RetentionSweep.keepAll()` (or `clearPendingExpiry()`), tested
    with a temp folder in `RetentionSweepTests`, and let the view model call one
    method. The CLI can then offer it too; the view model stays glue.

22. **minor**: `start-recording-from-main-window.md` step 8
    (`app.staticTexts["Recording"].firstMatch.exists`, "a row whose label begins with
    'Meeting '") and `floating-recording-indicator.md` step 8 (same "Meeting "
    assertion). The redesign removes the "Recording" chip from the entry (the state
    becomes "Recording · 12:34" in one `Text`) and F31 says the default title starts
    with "Call", not "Meeting". Both smoke tests written at steps 1 and 5 fail at
    step 8 or are wrong on day one. Pin them to identifiers the plans already define
    (`meeting-<uuid>` with the `isSelected` trait, a `meeting-<uuid>-state` value)
    rather than to copy.

23. **minor**: `first-run-feedback.md` preamble ("Nothing here widens the scope; every
    plan below restyles, surfaces or wires what v1 already has"). The 90 s auto-stop
    is a new behaviour (the device plan says so: "this is the feature the owner
    believed existed"), the floating bubble is a new surface beyond the scope's "menu
    bar shows a visible recording indicator", and the retention plan adds folder size
    measurement. All three are owner-approved on 2026-09-28 and small; the index
    should say "three additions approved by the owner on 2026-09-28: the auto-stop,
    the bubble, the disk usage line" instead of claiming none, so the scope document
    stays the authority it says it is.

24. **minor**: Open questions that are engineering risks, not owner decisions:
    redesign Q2 (CSS tokens), Q3 (preview source), Q6 (`toolbarBackground` with three
    columns); floating Q1 (`openWindow` from a hosted view), Q5 (Debug menu width);
    retention Q3 (sweep guard), Q5 (`steno sweep`); icon Q4 (Linux `--check` job);
    device change Q2 (aggregate sub-device behaviour). Move each to a "Risks and
    checks" list or into the step whose done-when settles it, so the Open questions
    sections read as the list the owner has to answer (accent hue, relative day
    labels, onboarding as a sheet, bubble hide setting, auto-stop toggle, keep only
    the mixdown, per-meeting delete, light plate).

25. **nit**: Dot sizes: 8 pt in `StopLabel` (start plan), 6 pt in the list entry and
    `MessageRow`, 5 pt for summary bullets, 6 pt in the chip recipe. The 2026-09-25
    app plan deferred a `StatusDot`; this round is where it pays for itself. One
    `StatusDot(color:)` at 6 pt, bullets at 4 pt (on grid; 5 is not).

26. **nit**: `macos-visual-redesign.md` "Spacing and radii": `radiusXL`, `radiusLG`,
    `radius`, `radiusSM`, `radiusXS` beside `Space.xs ... xxxl`. Two case styles for
    two ladders in one enum. `Theme.Radius.xl / lg / md / sm / xs` (16/12/8/6/4)
    reads like `Space` and lets the descent test iterate `allCases`.

27. **nit**: Accessibility ids for one action on four surfaces: `stop-recording`
    (menu bar), `sidebar-stop`, `stop-recording-header`, `bubble-stop`. Keep the
    existing `stop-recording`; name new ones `<surface>-<action>`: `header-stop`, not
    `stop-recording-header`. Same for `prompt-record` / `sidebar-record` /
    `record-call`: fine as is, but the header id is the odd one.

28. **nit**: Copy details. `device-change-during-recording.md`: "Audio devices
    changed; the recording continues." and "reconnecting…" use a semicolon in a UI
    string; house copy elsewhere uses two sentences ("Audio devices changed. Recording
    continues."). `floating-recording-indicator.md`: a 40 pt pill with radius 16 is
    not a pill; either radius 20 (true capsule, allowed as the circle exception) or
    keep 16 and call it a rounded bar in the spec. `macos-visual-redesign.md` decision
    8: -0.2 to -0.3 pt on 21 to 26 pt headings is about -0.01 em; design-craft's
    heading tracking is -0.025 em (-0.5 pt at 21). Either bend it on purpose or use
    the ladder's value.

29. **nit**: `device-change-during-recording.md` step 2: `DeviceChangeReason.synthetic`
    puts a test-only case in a public enum. The synthetic backend can report
    `.defaultInputChanged`; the tests do not need to know it was fake. Step 10:
    `Recording/EndReasonText.swift` holds label copy while every other label lives in
    `Design/Labels.swift` (`MeetingSource.label`, `PipelineStage.label`); put
    `RecordingEndReason.sentence` there.

30. **nit**: `macos-visual-redesign.md` Non-goals ("view models beyond pure display
    helpers") versus step 3 (`selectNext()`, `selectPrevious()`, `dayGroups`,
    `count(for:)` on `MeetingListViewModel`). Selection navigation is behaviour, and
    it is the right place for it; delete the non-goal line rather than bend it.

## What is good

- The ownership table in the index. One plan per shared thing, the others reference
  it; the findings above are mostly places where the table has a gap (header stack,
  tab-body copy), not where it is wrong.
- `RecordingControlPresentation.make(state:denied:)`, `FloatingContent.resolve`,
  `BubblePresentation.make` as `nonisolated` pure functions with one table test each.
  Per-surface presentation is the right shape; the surfaces show different subsets and
  a single status view model would carry every surface's nils. Only the copy strings
  need one owner (findings 10, 11).
- `Meeting.endReason` in core with migration `v3`, written by `LocalRecordingIntake.complete`.
  The CLI and the phone get the fact for free.
- The capture rebuild: session-orchestrated, coalesced, compared against a snapshot,
  bounded retries on the injected clock, silence for the gap, nothing on the IO
  thread, and the synthetic backend exercising the production path on CI.
- The menu bar diagnosis table: seven causes, each with a one-minute check, and a code
  change that helps under three of them without guessing which holds.
- Skipping the LLM passes instead of running fakes in the product, with the invariant
  ("`.ready` and `summary == nil` means no endpoint") pinned by a pipeline test rather
  than a column.
- Delivery-aware retention and the sweep skipping live meetings: a data-integrity fix
  found while answering a copy complaint.
- The redesign's `raised` correction, the achromatic accent, `StatusChip.neutral`,
  hairlines over shadows, the `ScrollView` list with `isSelected` traits, and the
  numeric verification checklist. The three-column decision is defensible for a Mac
  (list stays visible) once finding 3 is fixed; the nav column is thin, so the plan
  should be ready to fall back to two columns with the CTA in the list header if the
  first build shows 220 pt of mostly empty column.
- The steno pad icon: built from primitives two renderers agree on, tested for the
  empty-set trap, one script, palette from existing tokens.

## Sequencing

The index order is right in spirit and wrong in one place: the redesign's system
(tokens and components) is a dependency of steps 3, 4 and 6, but it ships in step 8,
so three plans build screens on components that are rewritten weeks later. Proposed
order, with the split from finding 5:

1. Start recording from the main window (unchanged; supplies `activeMeetingID`,
   `RecordingControlPresentation`, `RecordingViews.swift`). Pin its smoke test to ids
   (finding 22).
2. Redesign system PR: tokens (`macTokens`, `Space`, `Radius`), components
   (`Components.swift`, `Controls.swift`, `EmptyState.swift`), previews, the ladder and
   radius tests. No layout, no behaviour, no copy. Every later step composes from
   these.
3. Device change, core and capture PR (steps 1 to 6 of that plan) with `titleOrigin`
   folded into migration `v3` (finding 4). Independent of the app work and the longest
   to verify on a Mac; start it in parallel with 1 and 2.
4. Onboarding core PR (skip the fakes).
5. Retention (default, guard, Audio tab, header line per finding 8), built on the new
   components.
6. Onboarding app PRs (status rows, banner, page 2), with `summaryStatus` and
   `exportStatus` named as the selectors the redesign will keep (finding 2).
7. Floating indicator and prompt, with the shared `Countdown` (finding 9) and the
   achromatic bars (finding 1).
8. Device change, app PR (end reasons, auto-stop, the armed row on every surface).
9. App icon, any time; the bubble's `BubbleGlyph` switches when it lands.
10. Redesign layout PRs (steps 3 to 13 of that plan): display helpers, seeding, window
    and nav column with the collapse rule from finding 3, cards, detail pane with the
    owned header stack (finding 7), onboarding look, menu bar, sheet, settings,
    screenshots.

This keeps "last because it touches every screen" for the layout work, which is what
the index meant, while the components every sibling needs exist before the siblings
draw with them.
