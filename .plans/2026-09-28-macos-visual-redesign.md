# Steno: macOS visual redesign

Status: proposal, 2026-09-28. Triggered by first-run feedback.

Binding context: [`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md) (scope, the
four tabs, editable surfaces, non-goals) and
[`2026-09-25-macos-app-and-release.md`](2026-09-25-macos-app-and-release.md) (app
structure, view models, tests, CI). Siblings, all of which land before this plan:
[`2026-09-28-start-recording-from-main-window.md`](2026-09-28-start-recording-from-main-window.md)
owns the behaviour, labels, ids and shared views of the record control in the main window;
this plan owns where it sits and how it looks, and the detail header's Stop control.
[`2026-09-28-floating-recording-indicator.md`](2026-09-28-floating-recording-indicator.md)
owns the floating recording bubble and the detection prompt, and adds `radiusXL`,
`Motion.countdown` and `Motion.pulse`, which this plan reuses.
[`2026-09-28-onboarding-vault-and-llm.md`](2026-09-28-onboarding-vault-and-llm.md) owns the
onboarding window's pages, rows and copy; this plan owns its look.

This plan amends one line of the macOS plan: "Pixel design" stops being a non-goal, and
"tokens are dark-first and mirror `mobile/global.css`" becomes "token names and the
shared ladder mirror `mobile/global.css`; the Mac adds surface tokens the mobile app
does not need". The first implementation PR adds a one-line note under the macOS plan's
Non-goals pointing here.

## Goal

The owner ran the app for the first time and said the UI is ugly and does not match
Jamie (meetjamie.ai), the reference product. The screens work; they look like a
prototype: grey boxes on grey, a lone "All" picker, a trash can as the only toolbar
action, dense list rows, link-like tabs, cramped padding.

After this plan the app reads as one polished system in light and dark: a compact nav
column with a prominent Start recording control at the top, a card-based meeting list
grouped by day with title, time and a one-line summary preview, a calm detail pane with
a segmented tab control and a 720 pt reading column, hairline borders, soft 12 pt
corners, five text tiers, one achromatic accent, semantic colour only where state needs
it. Nothing is added to the product: no new screens, no new data, no editing of
generated text.

## Findings

The vocabulary is the design-craft system: a five-rung luminance ladder, alpha veils
over the canvas, radii descending 16 > 12 > 8 > 6 > 4, one functional tempo
(150 ms, cubic-bezier(.4,0,.2,1)), hairlines before shadows, hue only for status.

### Tokens and components (every screen inherits these)

| # | Where | What is wrong | Why it reads unpolished |
|---|---|---|---|
| F1 | `apps/macos/Steno/Design/Theme.swift:52-59` | `card`, `muted`, `secondary`, `accent` are black alpha veils in light mode (3.1 %, 5.9 %) | Veils brighten on a black canvas but darken on `#fafafa`, so every card is a grey box sunk into the page (onboarding screenshot). Jamie's light mode does the opposite: white cards raised on a faintly tinted canvas |
| F2 | `Theme.swift:122-123` | Radii are `radius = 8`, `radiusSmall = 5`; nothing at 12 or 16 | Buttons and cards cannot reach the soft 12 pt corners Jamie uses; 5 sits off the 4/6 ladder and looks like a default |
| F3 | `Theme.swift:117-121` | Spacing stops at `xl = 24`; no 32 or 48 | Screens use 16 and 24 everywhere, so nothing breathes; the detail header and the reading column sit tight against the edges |
| F4 | `apps/macos/Steno/Design/Components.swift:8-22` | Primary button: 13 pt semibold, flat `#171717`, radius 5, vertical padding 6 (about 25 pt tall) | A small heavy black block ("Run the test recording"). The recipe is 14 pt/500, 32 pt tall, radius 8 (12 for the hero CTA), a vertical `accent-from` to `accent-to` gradient |
| F5 | `Components.swift:44-57` | `StatusChip` is a `Capsule` with a 12 % tint for every role, including neutral ones ("Optional", tags, assignees) | Neutral metadata gets the same treatment as status, so everything looks like a status pill; the recipe is radius 6, `1px 6px` padding, hairline border for neutral chips, tint only for semantic state |
| F6 | `Components.swift:60-73` | `Card` fills with the veil (F1), radius 8, padding 12, no raised variant, no selected state | Cards cannot be white on light or carry selection; the list has to fall back to `List(.sidebar)` |
| F7 | `Components.swift:160-166` | `readingColumn()` pads 16 | The tab content starts 16 pt from the pane edge and the header; Jamie leaves 24 to 32 |
| F8 | `Theme.swift:103-113` | `lineHeight` is carried but never applied; body text uses the font's default leading | Summary bullets and transcript turns set tight; the 14/20 body rhythm is not there |

### Main window: sidebar, list, empty states

| # | Where | What is wrong | Why it reads unpolished |
|---|---|---|---|
| F9 | `apps/macos/Steno/Main/MainWindow.swift:19-21, 39` | Two-column `NavigationSplitView`; the sidebar is the meeting list; `.navigationTitle("Steno")` | The title bar says "Steno" next to a trash can. Nothing says "Meetings", nothing invites recording, and the most prominent affordance is destructive |
| F10 | `apps/macos/Steno/Main/MeetingListView.swift:97-108` | Delete is the only toolbar item | See F9. Jamie's toolbar carries search; delete lives behind the row's menu |
| F11 | `MeetingListView.swift:142-164, 72` | The state filter is a lone `Picker(.menu)` centred above the list with a full-width `Divider` under it | An orphan "All" control floating in the sidebar (screenshot). The tag picker appears beside it only when tags exist, so the row changes shape |
| F12 | `MeetingListView.swift:95` | `.searchable(placement: .sidebar)` | The system grey capsule dominates the top of the sidebar and fights the veil surfaces |
| F13 | `MeetingListView.swift:73-85, 184-214` | `List(.sidebar)` rows with separators hidden; `MeetingRow` packs title, status chip, `Sep 24, 10:02`, duration, source and tags into two lines at 14/12 pt | Dense, no grouping by day, no summary preview, a "Ready" chip on every finished meeting. Jamie shows one card per day with title, time and a one-line summary |
| F14 | `MeetingListView.swift:96` and the sidebar column material | `.background(Color.stenoBackground)` sits under the `NavigationSplitView` sidebar vibrancy | Two unrelated greys meet at the column edge (sidebar darker than the detail pane in the screenshot) |
| F15 | `MeetingListView.swift:166-181`, `MainWindow.swift:27-37` | Empty states are a 28 pt ghost symbol plus two lines of centred text | Reads as a placeholder. "Start a recording from the menu bar item." sends the user away from the window (the start-recording plan fixes the copy; this plan gives it a shape) |
| F16 | `MainWindow.swift:40`, `apps/macos/Steno/StenoApp.swift:26` | `minWidth 820`, default 1040 x 680 | Fine for two columns; three columns need a 960 pt minimum |

### Detail pane and tabs

| # | Where | What is wrong | Why it reads unpolished |
|---|---|---|---|
| F17 | `apps/macos/Steno/Main/MeetingDetailView.swift:351-374` | Tabs are plain text buttons; the active one gets a 5.9 % veil at radius 5; no container; a `Divider` runs under them | They read as links, not a control. The recipe is a segmented container (6 % veil, radius 8, 2 pt padding) with a raised active cell (radius 6, shadow-sm) |
| F18 | `MeetingDetailView.swift:260-323` | Header: 21 pt title, 12 pt meta, tags editor, "Review speakers" primary and an `ellipsis.circle` borderless menu labelled "Actions", all inside 16 pt padding | Cramped; the menu glyph floats without a hit shape; the meta row mixes five facts with 12 pt gaps and no separators |
| F19 | `MeetingDetailView.swift:328-329`, `apps/macos/Steno/Speakers/SpeakerReviewSheet.swift:157` | `.textFieldStyle(.roundedBorder)` | The AppKit bezel field inside a hairline design |
| F20 | `MeetingDetailView.swift:391-406, 409-441` | Footer shows `"\(delivery.destinationID): delivered"` and "Not delivered yet" in 12 pt faint | Raw identifiers ("obsidian: delivered") in the UI; the footer has no top rhythm |
| F21 | `apps/macos/Steno/Main/Tabs/SummaryTab.swift:65-85`, `TranscriptTab.swift:18-36`, `TasksTab.swift:29-53` | Body 14 pt with default leading; bullets are a "•" glyph at baseline; turn headers 13 pt semibold with three metadata texts in a row | Dense prose; no 14/20 rhythm; the transcript lane label ("Mic", "System") sits at 11 pt ghost next to the timestamp and looks like noise |
| F22 | `Tabs/ScratchpadTab.swift:12-22` | Editor fills with the veil, radius 8, padding 8 | Another grey box; the one editable surface should be the clearest raised surface on the page |

### Onboarding window

| # | Where | What is wrong | Why it reads unpolished |
|---|---|---|---|
| F23 | `apps/macos/Steno/Onboarding/OnboardingView.swift:16-18, 39` | The subtitle `Text` has no `.fixedSize(horizontal: false, vertical: true)` inside the fixed 520 pt frame | It truncates: "Audio never leaves this M..." (screenshot). A visible bug on the first screen anyone sees |
| F24 | `OnboardingView.swift:11, 38-39`, `StenoApp.swift:29-34` | Padding 24, row gap 8, width 520; the window keeps the standard title bar reading "Welcome to Steno" above an H1 reading "Welcome to Steno" | Duplicate title; content hugs the edges |
| F25 | `OnboardingView.swift:49, 57, 105-115` | Rows are `Card` veils (F1), the "Optional" chip is a grey capsule (F5), state glyphs render at 13 pt body size beside a 14 pt semibold title | Grey boxes on grey with mismatched glyph scale |

### Main window with a live recording selected (second screenshot)

The owner recorded a call and selected it: "main UI is fucking ugly, needs a proper
beauty pass". The screenshot adds these to F9 to F22.

| # | Where | What is wrong | Why it reads unpolished |
|---|---|---|---|
| F30 | `apps/macos/Steno/Main/MeetingListView.swift:73-84` (`List(selection:)` with `.listStyle(.sidebar)`), `MeetingRow` 184-214 | The selected row is the system accent blue. The green "Recording" chip (`Components.swift:120`, `live-bright` at 12 % over blue) and the `faint` meta text (`MeetingListView.swift:208-209`) sit on it unchanged | Green on blue and grey on blue are unreadable; the one coloured element on the screen is a selection colour the design never chose. Nothing in the plan's palette survives a system-accent highlight, so the highlight has to go (Selection styling below) |
| F31 | `Sources/StenoCore/Storage/LocalRecordingIntake.swift:164-178` (`defaultTitle`, "Call 2026-09-28 10:06"), shown verbatim at `MeetingListView.swift:190` and `MeetingDetailView.swift:263` | The storage default title is a machine string and it appears twice on screen, as the row title and as the 21 pt heading, above a meta line that repeats the same date and time a third time | A heading that is an ISO timestamp is the single loudest "prototype" signal in the window. The stored value is right for files and search; the screen needs a derived display title |
| F32 | `MeetingDetailView.swift:270-280` | Meta line "Sep 28, 2026 at 10:06   Call" in 12 pt `faint` with 12 pt gaps and no separators | Two facts of unequal kind side by side with no rhythm; while recording, the useful fact (elapsed time) is missing |
| F33 | `MeetingDetailView.swift:340-346` | "Add tags" is a plain 12 pt `faint` `Button` with no glyph, no hit shape, sitting alone on its row | Reads as stray text; the affordance is invisible |
| F34 | `MeetingDetailView.swift:351-374` | Tabs: text labels, the active one a grey pill (`secondary` veil, radius 5), 4 pt apart, a `Divider` under the row | Confirms F17 with real content: it looks like a nav bar of links, not a control that owns the pane below |
| F35 | `Components.swift:131-147` (`PendingText`, 14 pt medium `muted`), used at `SummaryTab.swift:17-19`, `TranscriptTab.swift:14-16`, `TasksTab.swift:13-14`; `readingColumn()` at `Components.swift:162-166` | "Summary appears after processing" left-aligned at heading weight, 16 pt from the top, in an otherwise empty pane | An empty state dressed as a heading. The pane has no centre, no icon, no state (the meeting is recording, the copy says processing) |
| F36 | `MainWindow.swift:39` | Window title "Steno" in the detail toolbar | Already F9; with a meeting selected it is now the third title on screen |
| F37 | `MeetingListView.swift:97-108` | Trash icon in the sidebar toolbar, enabled state depends on the selection | Already F10; next to a live recording it is a loaded gun with no label |
| F38 | Whole window | Every text block starts at the same 16 pt inset; the header, tabs and body share one left edge and one weight range; no surface separates the pane from the list except a hairline | No hierarchy, no whitespace: the eye has nowhere to land. Fixed by the type scale, the 32 pt header insets, the segmented control and the raised cards below |

### Menu bar popover, detection panel, speaker review, settings

| # | Where | What is wrong | Why it reads unpolished |
|---|---|---|---|
| F26 | `apps/macos/Steno/MenuBar/MenuBarView.swift:131-157, 180-196, 228-248` | 320 pt, 12 pt padding, buttons from F4, recent rows without hover or hit shape | Acceptable structure; it inherits the token and button fixes and needs row hit shapes |
| F27 | `apps/macos/Steno/Detection/DetectionPanel.swift:54-86` | Uses the popover fill and the two button styles | Inherits F4. Replaced by the floating recording indicator plan before this plan lands |
| F28 | `SpeakerReviewSheet.swift:387, 401, 464-479` | Fixed 560 x 520, veil cards, a horizontal scroller of secondary buttons for candidates | Busy; candidates should be quiet neutral chips, cards raised |
| F29 | `apps/macos/Steno/Settings/SettingsView.swift:31-49, 212-215, 116-117 vs 142-143` | Native `TabView` plus grouped `Form`: correct idiom. Only the capsule chip and inconsistent note tiers (`mutedForeground` here, `faint` there for the same role) | Light touch only |

## Non-goals

- New features or screens. No People, Tasks, Chats, Tags-as-objects, Shared, Ask AI,
  templates gallery or referral surfaces from Jamie's sidebar; cross-meeting chat is a
  scope non-goal. The nav column shows only what exists: the meeting list, its state
  filters, its tag filters, Settings.
- Editing summary, transcript or task text. Copy changes are limited to labels this plan
  names.
- Wiring the record control (mode choice, permissions, errors): the start-recording plan.
- The floating recording bubble and the detection prompt's look: the floating recording
  indicator plan. This plan only places a Stop control in the detail header and states
  that it shares the recorder with the bubble.
- Changing the stored meeting title, its format in exports or the Obsidian slug. The
  display title is a view-layer rule.
- A bundled font. The Mac uses the system font.
- Forcing light or dark. The app follows the system appearance; both must pass review.
- Changes under `mobile/`, to `Settings` structure, to view models beyond pure display
  helpers, or to StenoCore.
- Animations beyond the existing `Motion` tokens plus `Motion.pulse` from the floating
  indicator plan.

## Decisions

1. **Follow the system appearance; light is the first review target.** The plans call
   the tokens dark-first because the ladder was authored on black. That describes the
   authoring order, not the product: macOS defaults to light, the owner runs light, and
   every screenshot in the feedback is light. Both appearances ship; light is what gets
   reviewed first and must look right on its own terms, which means raised white
   surfaces, not darkened veils (F1).
2. **Keep the shared token names; add Mac-only surface tokens.** `Theme.tokens` keeps
   mirroring `mobile/global.css` and `ThemeTokensTests` stays as it is. A second list,
   `Theme.macTokens`, adds `sidebar` and `raised` (spec below) with its own resolve test.
   Reason: the phone recorder has no cards-on-canvas surface, so the CSS has nothing to
   mirror, and this plan must not touch `mobile/`.
3. **The accent stays achromatic.** No brand hue. The Start recording CTA uses the
   existing `accent-from` to `accent-to` gradient (`#171717` to `#000` in light, white
   to `#e4e4e7` in dark) with on-accent text; recording state is shown by the
   `destructive` dot and the `live` level bars, so the one hue budget goes to status.
   Reason: design-craft's one-hue rule, parity with mobile, and a black CTA on a light
   canvas is a proven premium idiom. Open question 1 keeps the door open.
4. **Three columns: nav, list, detail.** `NavigationSplitView(sidebar:content:detail:)`
   with `.balanced` style. Jamie navigates page-by-page; a Mac app keeps the list
   visible next to the meeting. The nav column collapses with the standard toggle.
5. **Filters become nav rows; tags become a nav group.** The state picker (F11) turns
   into four rows (All, In progress, Ready, Failed) with counts; tags list under a
   "Tags" label. Same `MeetingListViewModel.stateFilter` and `tagFilter`, new placement.
   This gives the nav column Jamie's structure using only existing behaviour.
6. **One card per day, entries inside.** Cards are grouped by calendar day; each entry
   is title, time, one-line preview, with a 2 pt rail on the left. Selection turns the
   rail `strong` and veils the entry. Deletion stays in the entry's context menu and the
   detail Actions menu; the toolbar loses the trash can.
7. **Hidden title bar, no window title.** `.windowStyle(.hiddenTitleBar)` on the main
   and onboarding windows; the toolbar keeps only the sidebar toggle. Each column paints
   its own opaque background so the split view's vibrancy never shows (F14).
8. **System font.** SF Pro at the token sizes; headings get `-0.2` to `-0.3` pt
   tracking, body and controls none.
9. **Radii ladder 16 / 12 / 8 / 6 / 4.** Cards and the CTA at 12, controls and inputs at
   8, chips and segmented cells at 6, tiny wells at 4. 16 is reserved for panels the
   system does not round for us.
10. **Hairlines, not shadows.** The only shadow is `shadow-sm` on the active segmented
    cell.
11. **Summary preview is derived, not stored.** `Meeting.previewLine` is the first line
    of `SummaryDocument.plainText` (the first bullet, `lead: text`), else a state word
    ("Recording", "Queued", "Processing", "Failed", "No summary"). Pure, tested, no new
    data.
12. **Settings stays native.** The complaint is the main window and onboarding; Apple's
    grouped form is what a Mac user expects in Settings.
13. **Display title is derived; the stored title stays.** `Meeting.displayTitle` returns
    the stored `title` unless it equals `LocalRecordingIntake.defaultTitle(source:
    startedAt:)` for the current time zone, in which case it renders "Call, Monday 10:06"
    (source label, comma, weekday for the last six days, else `MMM d`, then `HH:mm`).
    The stored value, exports, search and the Obsidian slug are untouched; the pipeline
    may still replace the title after processing (`Meeting.applyProcessingResults`) and
    the display follows. Reason: a timestamp heading is the loudest prototype signal on
    screen (F31), and a display-only rule needs no migration and no new data.
14. **The list is a `ScrollView`, not a `List`.** macOS 15 paints the system accent
    behind a selected `List` row in every list style and above `.listRowBackground`, so
    the plan's rail-plus-veil selection cannot coexist with it (Selection styling below).
15. **The detail header owns a Stop control while recording.** It calls the shared
    `RecordingController.stop()`, the same call the sidebar control, the menu bar item
    and the bubble make; this plan places and styles it, and the bubble does not replace
    it (floating indicator plan, decision 11).
16. **Record control semantics come from the start-recording plan.** Labels ("Record
    call", "Record in person", "Stop"), the call-first split button, the permission-denied
    state, the accessibility ids (`sidebar-record`, `sidebar-record-in-person`,
    `sidebar-stop`) and the `RecordingControl(controller:)` signature are decided in
    [`2026-09-28-start-recording-from-main-window.md`](2026-09-28-start-recording-from-main-window.md).
    This plan sets the box, fills, type and motion. The elapsed time lives inside the
    Stop button and there is no status line in the nav column; destructive is the dot and
    the "Stop" label on a `raised` surface, not a red fill, so the one hue stays a status
    signal.

## Design spec

### Colour tokens

Existing tokens keep their values (light on `#fafafa`: 18.97 / 9.93 / 7.49 / 5.26 /
2.42; dark on `#000`: 21 / 14.17 / 8.33 / 4.43 / 2.69). Roles are re-stated so the
engineer applies the right tier; two tokens are added.

| Token | Light | Dark | Role |
|---|---|---|---|
| `background` | `#fafafa` | `#000000` | List column, detail pane, onboarding canvas |
| `sidebar` (new, Mac) | `#f4f4f5` | `#0a0a0a` | Nav column fill |
| `raised` (new, Mac) | `#ffffff` | `rgba(255,255,255,0.031)` | Cards, scratchpad editor, active segmented cell, secondary button fill, inputs |
| `card` | `rgba(0,0,0,0.031)` | `rgba(255,255,255,0.031)` | Hover veil on rows and entries, empty-state icon well |
| `secondary` | `rgba(0,0,0,0.059)` | `rgba(255,255,255,0.059)` | Selected nav row, segmented tab container, selected entry veil |
| `border` | `rgba(0,0,0,0.122)` | `rgba(255,255,255,0.102)` | Every hairline, unselected entry rail |
| `ring` | `rgba(0,0,0,0.239)` | `rgba(255,255,255,0.251)` | Focused input border |
| `strong` | `#0a0a0a` | `#ffffff` | Titles, selected rail, active tab text |
| `foreground` | `#404040` | `#d4d4d4` | Body prose |
| `muted-foreground` | `#525252` | `#a3a3a3` | Times, secondary labels, inactive tabs |
| `faint` | `#696969` | `#737373` | Previews, meta rows, placeholders, notes (the one tier for all notes) |
| `ghost` | `#a3a3a3` | `#525252` | Decorative glyphs only, never text that carries meaning |
| `accent-from`, `accent-to`, `on-accent` | `#171717`, `#000000`, `#ffffff` | `#ffffff`, `#e4e4e7`, `#000000` | Primary buttons and the CTA (vertical gradient) |
| `popover` | `#ffffff` | `#101010` | Menu bar window, detection panel |
| `live`, `live-bright`, `warning`, `info`, `destructive` | unchanged | unchanged | Status chips, recording dot, level bars, message rows |

Dark mode note: `raised` and `card` coincide on black on purpose; the veil brightens
there, which is the dark idiom the ladder was authored on.

### Type scale

| Role | Token | Size / leading | Weight | Tracking | Tier |
|---|---|---|---|---|---|
| Onboarding H1 | `xxl` | 26 / 32 | semibold | -0.3 | strong |
| Detail title | `xl` | 21 / 28 | semibold | -0.2 | strong |
| Column title ("Meetings"), sheet title | `lg` | 18 / 23 | semibold | -0.2 | strong |
| Summary section heading | `base` | 16 / 23 | semibold | 0 | strong |
| Body prose, entry title, buttons, nav labels | `sm` | 14 / 20 (`lineSpacing(6)` on prose) | regular; medium for entry titles, buttons, nav labels | 0 | foreground / strong |
| Preview line, explanations, tab labels, search text | `xs` | 13 / 18 | regular; medium for tab labels | 0 | faint / muted |
| Times, meta rows, notes, counts, footer | `xxs` | 12 / 16 | regular; semibold for the date in card headers | 0 | muted / faint |
| Chips, section labels, lane labels | `xxxs` | 11 / 14 | medium; semibold + 0.6 tracking uppercase for section labels | 0 | per role |

Monospaced digits for every clock and timestamp.

### Spacing and radii

`Theme.Space` becomes: `xxs 2`, `xs 4`, `sm 8`, `md 12`, `lg 16`, `xl 24`, `xxl 32`,
`xxxl 48`, `hairline 1`. Radii: `radiusXL 16` (already added by the floating indicator
plan), `radiusLG 12`, `radius 8`, `radiusSM 6`, `radiusXS 4` (`radiusSmall` is removed;
call sites move to `radiusSM` or `radius`). Everything on the 4 pt grid except `xxs`, `hairline` and `radiusSM`.

Control heights: CTA 40, buttons 32, inputs 28, nav rows 32, icon buttons 28,
segmented cells 24 inside a 28 container.

### Components (`apps/macos/Steno/Design/`)

| Component | Box | Fill / border | Type | States |
|---|---|---|---|---|
| `StenoPrimaryButtonStyle` | height 32, padding 0 x 14, radius 8 | vertical gradient `accent-from` to `accent-to`, no border | 14 medium `on-accent` | pressed: opacity 0.95 and scale 0.98 over `Motion.functional`; disabled: opacity 0.5 |
| `StenoSecondaryButtonStyle` | same box | `raised`, hairline `border` | 14 regular `strong` | hover: `card` veil; pressed as primary |
| `RecordingControl` (the start-recording plan's `RecordingControl(controller:)`, restyled here; presentation, ids and actions unchanged) | full nav width minus 12 gutters, height 40, radius 12; split button: main segment plus a 28 pt trailing chevron segment separated by a hairline in `on-accent` at 20 % | idle: primary gradient; starting/stopping: same, label replaced by a 16 pt spinner; recording: `raised` with hairline `border`; permission denied: primary at opacity 0.5 with the reason `MessageRow(kind: .warning)` and the fix button below | idle: 16 pt `record.circle` glyph in a 28 pt well (radius 8, `on-accent` at 12 %) then "Record call" 14 medium `on-accent`; the chevron segment opens a menu with "Record in person"; recording: 8 pt `destructive` dot then "Stop" 14 medium `destructive` then elapsed `mm:ss` 14 mono `muted`, level bars 12 pt tall to the right | the dot pulses with `Motion.pulse`; disabled while starting/stopping |
| `NavRow` | height 32, padding 0 x 10, radius 8, gap 10 | selected: `secondary`; hover: `card` | 16 pt SF Symbol `muted` (selected `strong`), label 14 medium (`muted`, selected `strong`), optional trailing count 12 `faint` mono | id `nav-<name>` |
| `Card` | padding 16 (parameter), radius 12 | `raised`, hairline `border`, no shadow | content | none |
| `MeetingCard` (one per day) | `Card` with padding 16 | as `Card` | header: date `MMM d` 12 semibold `strong`, " / " and weekday `EEEE` 12 `faint`; 12 pt below; entries stacked with 12 pt gaps | none on the card |
| `MeetingEntry` | HStack gap 12: 2 pt rail (radius 1, full entry height) + VStack gap 2; padding 8 x 8, radius 8; `contentShape` is the whole entry | rail `border`, selected `strong`; selected entry `secondary` veil; hover `card` veil | title 14 medium `strong` 1 line tail-truncated; time `h:mm` 12 `muted` (mono digits), status chip trailing only for recording / queued / processing / failed; preview 13 `faint` 1 line | context menu: Delete Meeting… (disabled while recording or processing); id `meeting-<uuid>` retained |
| `SegmentedTabs` | container padding 2, radius 8, height 28; cells padding 4 x 10, radius 6 | container `secondary`; active cell `raised` + `shadow-sm` (0 1 3 0 black 10 %, 0 1 2 -1 black 10 %) | 13 medium `strong` active, 13 regular `muted` inactive | swap over `Motion.functional`; ids `tab-<raw>` and the `isSelected` trait retained |
| `StatusChip` | padding 1 x 6, radius 6, gap 4 | semantic: colour at 12 % fill, text in the colour; neutral (`.neutral` case): transparent, hairline `border`, text `muted` | 11 medium | none |
| `IconButton` | 28 x 28, radius 14 | transparent, hairline `border`; hover `card` | 14 pt glyph `muted`, hover `strong` | requires an accessibility label |
| `SearchField` | height 28, padding 0 x 10, radius 8, gap 8 | `raised`, hairline `border`; focused `ring` | 14 pt `magnifyingglass` `faint`, text 13 `strong`, placeholder `faint` | `@FocusState`; ⌘F from `AppCommands` focuses it; id `search-meetings` |
| `StenoTextFieldStyle` | height 28, padding 0 x 10, radius 8 | `raised`, hairline; focused `ring` | 13 `strong`, placeholder `faint` | replaces every `.roundedBorder` |
| `EmptyState` | centred VStack gap 12; icon well 48 x 48 radius 12 | well `card` + hairline; 20 pt symbol `faint` | title 14 medium `strong`; body 13 `muted`, max width 280, centred; optional action 16 below | ids passed in (`empty-meetings`, `empty-detail`) |
| `SectionLabel` | unchanged | | 11 semibold uppercase tracking 0.6 `faint` | |
| `MessageRow` | padding 8 x 10, radius 8 | colour at 8 % fill | 6 pt dot, 13 `foreground` | |

Motion: every state swap uses `Motion.functional`; the recording dot pulse
(`Motion.pulse`) is the one ambient animation and honours Reduce Motion by holding at
opacity 1.

### Per-screen layout

**Main window** (`StenoApp.swift`, `MainWindow.swift`): `.windowStyle(.hiddenTitleBar)`,
`.toolbar(removing: .title)`, `.toolbarBackground(.hidden, for: .windowToolbar)`,
`minWidth 960, minHeight 600`, default 1120 x 720. Three columns:

1. **Nav column** (`Main/NavigationColumn.swift`), width min 200 ideal 220 max 260,
   fill `sidebar`, right hairline. From the top: 8 pt below the toolbar, `RecordingControl`;
   24 pt; `SectionLabel("Meetings")` at 10 pt inset; rows All (`rectangle.stack`),
   In progress (`clock`), Ready (`checkmark.circle`), Failed (`exclamationmark.triangle`)
   with counts from `MeetingListViewModel.all`; 24 pt; when tags exist,
   `SectionLabel("Tags")` and one row per tag (`tag` glyph, label `#name`), selecting a
   row sets `tagFilter`, selecting it again clears it; `Spacer`; hairline; row Settings
   (`gearshape`) that calls `openSettings`, 8 pt bottom padding. Everything scrolls if
   the tag list overflows; the CTA and the Settings row stay pinned.
2. **List column** (`MeetingListView.swift`, `Main/MeetingCard.swift`), width min 320
   ideal 380 max 480, fill `background`, right hairline. Header: 24 pt top, 16 pt sides:
   title 18 semibold ("Meetings", or the state filter title, or `#tag`); 12 pt;
   `SearchField`; 16 pt; then a `ScrollView` of `MeetingCard`s with 16 pt side insets
   and 12 pt gaps, 24 pt bottom. The column is focusable; up and down arrows move the
   selection through entries (`onMoveCommand`), the selection scrolls into view. Error
   text (`model.error`) appears as a `MessageRow` under the search field. Empty:
   `EmptyState` centred in the scroll area (`waveform`, "No meetings yet", body from the
   start-recording plan, no action button because the control sits above; or "No meetings
   match" with a secondary "Clear filters" that resets query, state and tag).
3. **Detail pane** (`MeetingDetailView.swift`), min 480, fill `background`. Header at 32
   pt sides, 24 pt top: title row (`displayTitle` 21 semibold, `textSelection`, trailing
   status chip only for queued, processing and failed, the Stop control while
   recording, see Recording and processing states); 6 pt; meta row 12 `faint` joined by " · " (full date, time,
   duration `clockText`, source label, language name, token count); 12 pt; tags row:
   neutral chips, then an "Add tag" ghost button (12 `faint`, `plus` glyph 10 pt) that
   swaps to a `StenoTextFieldStyle` field 240 wide; trailing on the same row: "Review
   speakers (n)" primary when needed, then `IconButton("ellipsis")` opening the existing
   Actions menu; failed reason and `model.error` as `MessageRow`s under the title. 16 pt;
   `SegmentedTabs` left-aligned at 32 pt inset; 16 pt; full-width hairline; content;
   full-width hairline; footer at 32 pt sides, 12 pt vertical: one chip per delivery
   (neutral chip with `folder` glyph, destination display name and "Delivered 10:02";
   `info` while pending; `destructive` "Failed" with the message as help), or "Not
   delivered yet" 12 `faint`; trailing spinner while busy. Empty: `EmptyState`
   (`text.alignleft`, "Select a meeting", "Pick a meeting on the left to read its summary,
   transcript and tasks.").

**Tabs** (`Main/Tabs/*.swift`): `readingColumn()` becomes max width 720, padding 32
sides, 24 top, 32 bottom. Summary: section heading 16 semibold with 24 pt above (0 for
the first), bullets as an HStack of a 5 pt `faint` dot at 8 pt top offset and 14/20
prose (`lineSpacing(6)`), 8 pt between bullets. Transcript: turns 20 pt apart; header
is speaker 13 semibold `strong`, timestamp 12 mono `faint`, lane as a neutral chip only
for `.macCall` meetings; text 14/20. Tasks: rows 8 pt apart, 16 pt `square` or
`checkmark.square` glyph, text 14, meta chips neutral, priority chip semantic. Scratchpad:
editor in a `raised` hairline surface, radius 12, padding 12, min height 240, text
14/20; hint 12 `faint` below. Pending and failed bodies follow the Recording and
processing states table.

**Onboarding** (`OnboardingView.swift`, `StenoApp.swift`), both pages of the onboarding
plan: `.hiddenTitleBar`, content width 560, padding 40 top, 32 sides and bottom. Step
caption 12 `faint`; 4 pt; H1 26 semibold tracking -0.3; 6 pt; subtitle 14 `muted` wrapping
(`fixedSize(horizontal: false, vertical: true)`), the retention sentence beneath it 13
`faint`; 24 pt; step cards (`Card`, padding 16, gap 12): row of a 24 pt frame holding an 18 pt state
glyph (`checkmark.circle.fill` `live-bright`, `xmark.circle.fill` `destructive`,
`circle` `ghost`), title 14 semibold `strong`, neutral "Optional" chip, trailing
"Skipped" 12 `faint`; expanded: 12 pt, explanation 13 `muted` `lineSpacing(4)`, 12 pt,
action row (primary, secondary, ghost "Skip" 13 `faint`), spinner and the listening note
12 `faint` while requesting; the setup rows' fields use `StenoTextFieldStyle`. 24 pt;
footer right-aligned, buttons per the onboarding plan (Later or Done on page 1, Back and
Finish on page 2; secondary and primary).

**Menu bar** (`MenuBarView.swift`): 320 wide, padding 16, fill `popover`. Status row
unchanged; level bars unchanged; buttons: "Record call" primary with a 14 pt
`record.circle` glyph, "Record in person" secondary, "Stop" in the same destructive
treatment as the sidebar control (`raised` surface, `destructive` dot and label). Queue rows and
recent rows: padding 6 x 8, radius 6, hover `card`, whole row hittable. Footer text
buttons 13 `muted`, hover `strong`.

**Detection prompt and recording bubble**: already restyled by
`2026-09-28-floating-recording-indicator.md` (`Panels/`); they inherit the button styles
through the shared components and nothing else here touches them.

### Recording and processing states

One line of copy per state, one visual per state, the same words in the list entry, the
detail header and the tab bodies. Elapsed time comes from
`RecordingController.recording` (`.recording(since:)`) through a `TimelineView` at 1 s,
as `MenuBarView.swift:170-174` does today; processing stage names come from
`PipelineStage.label` (`Labels.swift:17-33`) via the queue the menu bar already reads.

| State | List entry | Detail header | Tab bodies |
|---|---|---|---|
| Recording | Title `displayTitle`; time row shows the start time, then a `destructive` 6 pt dot pulsing with `Motion.pulse` and "Recording · 12:34" in 12 pt mono `muted`; no chip; preview line omitted | Title row: `displayTitle` 21 semibold; trailing, in place of the status chip, a "Stop" secondary button (32 pt, radius 8, `raised` with hairline, 8 pt `destructive` dot before the label, elapsed `mm:ss` mono after it, id `stop-recording-header`) that calls the shared `RecordingController.stop()`. Meta line: full date, start time, source label. Below the meta line the two `LevelBars` (`MenuBarView.swift:293-331`) at 4 pt height and 240 pt width, labels 11 `faint`, fills `live-bright` | Every tab: `EmptyState` centred in the pane, well glyph `waveform`, title "Recording", body "The summary, transcript and tasks appear a few minutes after you stop." Scratchpad stays editable (notes during the call are in scope) with the same hint under the editor |
| Queued | `info` chip "Queued" trailing the title; time row start time only; preview "Waiting to process" 13 `faint` | Status chip `info` "Queued"; meta line adds duration `clockText`; no level bars, no Stop | `EmptyState`, glyph `clock`, title "Queued", body "Processing starts when the current meeting finishes." |
| Processing | `info` chip "Processing"; preview shows the stage label ("Transcribing", "Finding speakers", ...) 13 `faint` | Status chip `info` with the stage label; under the meta line a 240 pt linear `ProgressView(value:)` tinted `strong` with the stage label 12 `faint` beside it (the queue fraction the menu bar shows) | `EmptyState`, glyph `waveform.badge.magnifyingglass`, title = stage label, body "Audio stays on this Mac. This usually takes a minute or two.", a small spinner under the body |
| Failed | `destructive` chip "Failed"; preview shows the first line of the reason 13 `faint` | Status chip `destructive` "Failed"; the reason as a `MessageRow(.error)` under the title row; "Re-run summary" stays in the Actions menu | `EmptyState`, glyph `exclamationmark.triangle`, title "Processing failed", body = first sentence of the reason, a secondary "Try again" button that calls the existing `rerunSummary()` (disabled while busy) |
| Ready, no content in a tab | as spec | as spec | `EmptyState` without the well: title "No summary" / "No transcript" / "No tasks" 14 medium `strong`, body 13 `muted` ("The template produced no sections." / "No speech was recognised." / "No tasks were found.") |

`PendingText` (`Components.swift:131-147`) becomes a thin wrapper that picks the row of
this table from `Meeting.state`, so `TabText.lines` and `TabTextSnapshotTests` keep one
source for the words; the snapshot fixture is updated once with the new copy.

### Selection styling

The blue in F30 is `NSTableView`'s selection, which SwiftUI's `List` paints for every
`listStyle` on macOS 15 and draws above `.listRowBackground`. `.listRowBackground`
therefore cannot cover it, `.tint` and `.accentColor` only recolour it,
`.selectionDisabled` removes selection altogether, and `.listStyle(.plain)` keeps it.
The list column is a `ScrollView` with a `LazyVStack` of `MeetingCard`s and manual
selection:

- `MeetingEntry` is a `Button(action:)` with `.buttonStyle(.plain)`, `contentShape` of
  its full 8 pt padded frame, `.accessibilityAddTraits(isSelected ? [.isSelected] : [])`,
  `.accessibilityIdentifier("meeting-<uuid>")`; the click sets `model.selection`.
- The selected entry draws the 2 pt rail in `strong` and a `secondary` veil at radius 8;
  hover draws a `card` veil; both swap over `Motion.functional`. No system colour is
  used anywhere in the column.
- The column root is `.focusable()`, `.focusEffectDisabled()`, `.onMoveCommand` moves
  the selection to the previous or next entry across day boundaries in `dayGroups`
  order, `.onKeyPress(.return)` is a no-op (the detail already follows selection),
  `.onDeleteCommand` sets `pendingDeletion` for the selected meeting (the confirmation
  dialog is unchanged). A `ScrollViewReader` scrolls the selected id into view with
  `Motion.spatial`.
- Accessibility: the column has `.accessibilityElement(children: .contain)`,
  `.accessibilityLabel("Meetings")`, `.accessibilityIdentifier("meeting-list")`; each
  day card is a container with the date as its label, so VoiceOver reads "Sep 28,
  Monday, group" then the entries; the `isSelected` trait on the entry is what
  `LaunchSmokeTests` and VoiceOver both read.
- The nav column uses the same pattern (`NavRow` buttons, no `List`), so the window has
  no system-accent selection at all. The detail `SegmentedTabs` already carries
  `isSelected` per cell.

**Speaker review sheet** (`SpeakerReviewSheet.swift`): 600 x 560, padding 24. Title 18
semibold with "n to review" 13 `faint`; cards `Card` padding 16 gap 12; play control as
a 28 pt `IconButton`; suggestion rows unchanged in structure; candidates as neutral
chip buttons (hairline, radius 6, 12 medium, height 24, hover `card`) in the existing
horizontal scroller; name field `StenoTextFieldStyle`; merge picker unchanged; footer
Later / Done.

**Settings** (`SettingsView.swift`): structure unchanged. Chips through the new
`StatusChip`; every explanatory note uses `faint` at 13; the QR image gets radius 8 and
a hairline.

## Implementation steps

Each step is one PR or one commit in a PR, in this order. Build and test commands are in
Verification.

1. **Tokens.** `apps/macos/Steno/Design/Theme.swift`: add `sidebar` and `raised` as
   `Theme.macTokens`, extend `Space`, add the radii below `radiusXL`, add
   `Color.stenoSidebar`, `Color.stenoRaised`. `apps/macos/StenoTests/ThemeTokensTests.swift`: a test that every
   `macTokens` entry resolves in both appearances and that no `macTokens` name collides
   with a CSS name; a test that every `Space` value except `xxs`, `hairline` and
   `radiusSM` is a multiple of 4 and that the radii strictly descend 16 > 12 > 8 > 6 > 4.
   Done when `StenoTests` passes and the existing CSS mirror test is untouched.
2. **Components.** `Components.swift`: rewrite the two button styles, `Card(padding:)`,
   `StatusChip` with a `.neutral` variant, `MessageRow`, `readingColumn()`. New files
   `Design/Controls.swift` (`IconButton`, `SearchField`, `StenoTextFieldStyle`,
   `SegmentedTabs`, `NavRow`) and `Design/EmptyState.swift`. Add `#Preview` blocks per
   component showing light and dark side by side. Done when the app builds, the
   `TabTextSnapshotTests` still pass, and the previews render both appearances.
3. **Pure display helpers.** `apps/macos/Steno/Design/Labels.swift`:
   `Meeting.displayTitle(now:calendar:timeZone:)`, `Meeting.previewLine`,
   `MeetingState` copy for the states table, `MeetingListViewModel.StateFilter.symbolName`.
   `apps/macos/Steno/Main/MeetingListViewModel.swift`: `dayGroups: [DayGroup]`
   (day start in the current calendar, meetings newest first inside the day),
   `count(for: StateFilter)`, `selectNext()` and `selectPrevious()` over the flattened
   groups. Tests in `apps/macos/StenoTests/MeetingListViewModelTests.swift` (grouping
   across a midnight boundary, counts, next/previous across a day boundary, preview line
   for a summarised, a processing and a failed meeting) and a new
   `apps/macos/StenoTests/DisplayTitleTests.swift` (default title today renders "Call,
   Monday 10:06"; a default title eight days old renders "Call, Sep 20 10:06"; a
   calendar title renders unchanged; a default title in another time zone is still
   recognised; the stored `title` is never mutated). Done when the tests pass.
4. **Preview seeding.** `apps/macos/Steno/AppEnvironment.swift` `preview()`: seed four
   more synthetic meetings over three days (one processing, one failed, two ready with
   two-bullet summaries) so screenshots and UI tests exercise grouping; keep the fixture
   meeting the newest. Honour a `-steno-empty` launch argument that seeds nothing. Done
   when `AppEnvironmentTests` builds both roots and `LaunchSmokeTests` still finds
   "Produktstrategie 90/10".
5. **Window and nav column.** `StenoApp.swift` (window style, sizes, ⌘F Find command),
   `MainWindow.swift` (three columns, `.balanced`, empty detail via `EmptyState`), new
   `Main/NavigationColumn.swift` (`RecordingControl` moved in from `MainWindow`, filter and
   tag rows, Settings row), `MeetingListView.swift` (remove the picker row, the toolbar
   item and `.searchable`). `RecordingControl` keeps the start-recording plan's
   presentation struct, ids and actions and only changes geometry, fills and type. Done
   when `LaunchSmokeTests` passes and new assertions hold: `app.buttons["sidebar-record"]`
   sits above `app.buttons["nav-failed"]` (compare frames), clicking `nav-failed` hides the fixture meeting and clicking
   `nav-all` shows it again, `app.textFields["search-meetings"]` exists and
   `app.buttons["delete-meeting"]` does not.
6. **Meeting cards and selection.** New `Main/MeetingCard.swift` (`MeetingCard`,
   `MeetingEntry`); `MeetingListView.swift` drops `List` for the `ScrollView` described
   under Selection styling and renders the header, `SearchField`, the cards, keyboard
   selection and the empty states. Done when the smoke test passes, no `List` remains in
   `apps/macos/Steno/Main/`, a new UI assertion presses the down arrow with the list
   focused and sees the detail title change, the selected entry exposes the `isSelected`
   trait (`app.buttons["meeting-<id>"].isSelected`), and `app.staticTexts` contains a
   weekday name for the fixture day.
7. **Detail pane and tabs.** `MeetingDetailView.swift`, `Tabs/SummaryTab.swift`,
   `TranscriptTab.swift`, `TasksTab.swift`, `ScratchpadTab.swift`, `Components.swift`
   (`PendingText` over the states table), `Labels.swift` (a `displayName` for the
   Obsidian destination id used by `DeliveryBadge`). Done when the four `tab-*` clicks in
   the smoke test still find their content, `TabTextSnapshotTests` is regenerated once
   for the new pending copy and then stable, the raw `destinationID` string appears
   nowhere in a `Text`, and the raw default title appears nowhere on screen for the
   seeded recording meeting.
7a. **Recording and processing states.** `MeetingDetailView.swift` (header Stop control,
   level bars, progress row), `Main/MeetingCard.swift` (entry variants),
   `Design/EmptyState.swift` variants. The Stop control calls
   `controller.recorder.stop()` and renders the start-recording plan's `StopLabel`; it
   is disabled while `.starting` or `.stopping`. The preview
   environment seeds one meeting in each of recording (with a fake session so the
   recorder reports `.recording(since:)`), queued, processing and failed. Done when a UI
   test selects the recording meeting and finds `stop-recording-header` and the
   "Recording" empty-state title, selects the failed meeting and finds "Processing
   failed", and `RecordingControllerTests` is unchanged.
8. **Onboarding.** `OnboardingView.swift`, `StenoApp.swift` (window style). Honour a
   `-steno-show-onboarding` argument in the UI-testing environment that opens the window
   with all permissions unknown. Done when a UI test opens it and finds the full subtitle
   and the four page 1 step titles, `OnboardingViewModelTests` is unchanged, and the light and dark
   previews show white cards on the canvas with no truncated text.
9. **Menu bar.** `MenuBarView.swift` only. Done when `MenuBarViewModelTests` is unchanged and
   the previews show hover veils on queue and recent rows.
10. **Speaker review sheet.** `SpeakerReviewSheet.swift`. Done when
    `SpeakerReviewViewModelTests` is unchanged and no `.roundedBorder` remains in
    `apps/macos/Steno`.
11. **Settings.** `SettingsView.swift`. Done when `SettingsViewModelTests` is unchanged
    and every note in the file uses `faint`.
12. **Screenshot evidence.** `apps/macos/StenoUITests/LaunchSmokeTests.swift` attaches
    `XCTAttachment(screenshot:)` of the main window after selection and of the
    onboarding window, `lifetime = .keepAlways`, so the `ui-smoke` job's uploaded
    `.xcresult` carries reviewable images. Done when
    `xcrun xcresulttool export attachments` on the CI artefact yields both PNGs.
13. **Plan bookkeeping.** Add the amendment note under Non-goals in
    `.plans/2026-09-25-macos-app-and-release.md`. Same PR as step 1.

## Verification

- CI: `.github/workflows/swift-ci.yml` job `app` (Forge via `vars.MACOS_RUNS_ON`, else
  `macos-15`) generates the project, builds `Steno`, runs `StenoTests`; job `ui-smoke`
  (hosted `macos-15`) runs `LaunchSmokeTests` and uploads `apps/macos/build/*.xcresult`.
  Both must be green for every step.
- Local or Forge: `xcodegen generate --spec apps/macos/project.yml`, then
  `xcodebuild -project apps/macos/Steno.xcodeproj -scheme Steno -configuration Debug
  -destination platform=macOS,arch=arm64 build CODE_SIGN_IDENTITY=- CODE_SIGN_STYLE=Manual
  DEVELOPMENT_TEAM=`, then launch the built `Steno.app` with `--args -steno-ui-testing`
  for the seeded preview environment, `-steno-empty` for the empty states,
  `-steno-show-onboarding` for onboarding. Switch appearance in System Settings >
  Appearance and capture each window with `screencapture -l <windowid> out.png`.
- Visual review, numeric, per the design-craft REVIEW mode: at most five neutral text
  tiers on a screen; hue only on status chips, the recording dot and level bars; every
  surface is `raised`, a veil, or a hairline; radii nest 12 > 8 > 6; gaps are 8, 12, 16,
  24, 32; no text truncates in the default window size; light and dark both pass.
- Side by side with the Jamie screenshots from the feedback: CTA at the top of the nav
  column, nav rows with glyphs, cards with date header, rail, title, time, preview.
- The second screenshot's checklist, with a live recording selected in the seeded
  preview environment: no system blue anywhere; the selected entry shows a `strong` rail
  and a veil; the heading reads "Call, <weekday> <time>", not the ISO string; the header
  shows the Stop control with elapsed time and two level bars; every tab body is a
  centred `EmptyState` whose title matches the meeting state; "Steno" and the trash can
  are gone from the toolbar.

## Open questions

1. Accent hue. The plan keeps the achromatic accent. If the owner wants a signature
   colour like Jamie's purple, retarget per the design-craft accent rules (one hue, L/C
   relationship kept, contrast re-verified in both themes) in a follow-up plan that also
   updates `mobile/global.css` so the two apps agree.
2. Should `sidebar` and `raised` also be added to `mobile/global.css` so the CSS stays the
   single source? This plan keeps them Mac-only to stay out of `mobile/`.
3. Summary preview source: first bullet of the whole document, or the first bullet of a
   named section when the template has one (`SummaryTemplate.bundled` section ids)? The
   plan takes the first bullet; revisit once templates settle.
4. Relative day labels ("Today", "Yesterday") in card headers instead of the date. Jamie
   shows dates; the plan follows Jamie.
5. Onboarding as a sheet over the main window instead of a second window. Not changed
   here; the separate window is what the app already has and it opens before the main
   window is useful.
6. Whether `NavigationSplitView` on macOS 15 reliably honours `.toolbarBackground(.hidden,
   for: .windowToolbar)` with three columns, or whether the nav column needs a
   `ZStack` over an opaque `NSVisualEffectView`-free background. Confirm on the first
   build; either way the nav column ends up opaque `sidebar`.
7. Display title window: weekday for the last six days, then `MMM d`. Jamie uses the
   calendar event title and otherwise "Meeting" plus time; if the owner prefers the
   calendar title only, `displayTitle` collapses to the stored title.
