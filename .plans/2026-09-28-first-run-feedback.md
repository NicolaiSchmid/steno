# Steno: first-run feedback, program note

Status: proposal, 2026-09-28, revised after the 2026-09-28 reviews. Index for the seven plans
triggered by the owner's first run of the macOS app (two rounds of feedback on 2026-09-28).
Binding context: [`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md) and
[`2026-09-25-macos-app-and-release.md`](2026-09-25-macos-app-and-release.md). Six of the seven
plans restyle, surface or wire what v1 already has. Three additions widen the scope, each small
and approved by the owner on 2026-09-28: the 90 s auto-stop after the call app closes the
microphone (device-change plan, Decision 8; its PR adds one line to the scope's Capture (Mac)
list), the floating recording bubble (a new surface beyond the scope's "menu bar shows a
visible recording indicator"), and the audio folder's disk usage line (retention plan,
Decision 5). Nothing else here changes the scope document.

## Status (2026-09-29)

Every plan in this note is implemented and merged, in the order below; the numbers are the PRs.

| Plan | PRs |
|---|---|
| This note and the seven plans | #103 |
| [`2026-09-28-start-recording-from-main-window.md`](2026-09-28-start-recording-from-main-window.md) | #110 |
| [`2026-09-28-onboarding-vault-and-llm.md`](2026-09-28-onboarding-vault-and-llm.md) | #112 (optional LLM passes), #127 (onboarding setup page) |
| [`2026-09-28-device-change-during-recording.md`](2026-09-28-device-change-during-recording.md) | #115 (capture), #125 (auto-stop and end reasons) |
| [`2026-09-28-app-icon.md`](2026-09-28-app-icon.md) | #122 |
| [`2026-09-28-audio-retention-keep-forever.md`](2026-09-28-audio-retention-keep-forever.md) | #123 |
| [`2026-09-28-macos-visual-redesign.md`](2026-09-28-macos-visual-redesign.md) | #124 (design system), #128 (window and cards), #131 (detail pane and onboarding look), #130 (menu bar), this PR (screenshots in both appearances) |
| [`2026-09-28-floating-recording-indicator.md`](2026-09-28-floating-recording-indicator.md) | #126 |

What remains are owner decisions, not work: the open questions of each plan, gathered in the
redesign plan's "Open questions" (accent hue, relative day labels, onboarding as a sheet, the
display-title window, midnight refresh) and the icon plan's brand-hue question, to be decided
together if at all.

## Feedback and where it is answered

| Feedback (2026-09-28) | Finding | Plan |
|---|---|---|
| "The UI is super ugly", "does not match Jamie" | Light mode paints black alpha veils on a near-white canvas, so every card is a grey box; radii and spacing are off-ladder; the list is a dense system sidebar with no date grouping or preview; the onboarding subtitle truncates | [`2026-09-28-macos-visual-redesign.md`](2026-09-28-macos-visual-redesign.md) |
| "I can't start a meeting from the sidebar" | Recording starts only from the menu bar, the Record menu (⌘⇧R) and the detection prompt; the main window never renders the recorder although it holds it | [`2026-09-28-start-recording-from-main-window.md`](2026-09-28-start-recording-from-main-window.md) |
| "Onboarding didn't ask for vault or LLM. Will it be flagged?" | No. With nothing configured the app substitutes test doubles: the first meeting shows a fabricated summary and a footer that reads like "pending". Delivery is skipped without a row | [`2026-09-28-onboarding-vault-and-llm.md`](2026-09-28-onboarding-vault-and-llm.md) |
| "A setting to save the recordings (no deletion)" | The setting exists in Settings > Audio ("Keep forever" is a real case), but the default deletes after 30 days and nothing says so; audio can also expire after a failed export | [`2026-09-28-audio-retention-keep-forever.md`](2026-09-28-audio-retention-keep-forever.md) |
| "The detection prompt needs to be redesigned", "we need a live recording indicator as a hovering bubble, the menu bar doesn't seem to work" (second round) | The prompt is a plain card with a countdown label; recording state is visible only in the menu bar item and the main window; the menu bar failure is diagnosed in the plan | [`2026-09-28-floating-recording-indicator.md`](2026-09-28-floating-recording-indicator.md) |
| "Main UI needs a proper beauty pass" (second round, with a recording selected) | System-blue selection swallows the chip, raw auto-titles, link-style tabs, placeholder copy at heading weight | folded into the redesign plan |
| "It auto-stops recording when the meeting mic permissions stop" (second round) | Not a feature. The detector only dismisses the prompt on mic release; the recording ended through the device-loss path when a default audio device changed at call end, and the only explanation is a warning line in the menu bar | [`2026-09-28-device-change-during-recording.md`](2026-09-28-device-change-during-recording.md) |
| "It's not asking for API keys or missing onboardings" (second round) | Confirms the onboarding finding: the meeting is processed with the placeholder summariser | onboarding plan above |
| "Please design an icon" | The app icon set lists ten sizes and contains no images | [`2026-09-28-app-icon.md`](2026-09-28-app-icon.md) |

## Order

The redesign's system (tokens and components) is a dependency of the retention, onboarding,
floating and device-change app work, so it ships second; the redesign's layout work ships last
because it touches every screen.

1. Start recording from the main window. Smallest change, unblocks the owner's testing;
   supplies `activeMeetingID`, `RecordingControlPresentation`, `AppController.startRecordingFromWindow`,
   `Recording/RecordingViews.swift` (achromatic `LevelBars`, `StatusDot`). Its smoke test pins ids,
   not copy.
2. Redesign, system PR (steps 1, 2 and 13 of that plan): `macTokens`, `Space`, `Theme.Radius`,
   the rewritten components, `Controls.swift`, `EmptyState.swift`, previews, the ladder and
   descent tests. No layout, no behaviour, no copy. Every later step composes from these.
3. Device change, core and capture PR (steps 1 to 6 of that plan), with `Meeting.titleOrigin`
   folded into migration `v3` beside `endReason`. Independent of the app work and the longest to
   verify on a Mac; start it in parallel with 1 and 2.
4. Onboarding, core PR (steps 1 to 3 of that plan): stop running the fake cleaner and summariser
   in the product so the next test recording produces honest output.
5. Audio retention: default to keep forever for new installs, the delivery guard,
   `RetentionSweep.keepAll()`, the Audio tab, the header line, `OnboardingViewModel.init(environment:)`.
   The owner flips the stored 30-day row once.
6. Onboarding, app PRs (steps 4 to 10 of that plan): status rows selected by `summaryStatus` and
   `exportStatus`, the banner, the second onboarding page.
7. Floating recording indicator and detection prompt. Depends on step 1; ships before the
   redesign's layout because the owner cannot see recording state today. Adds `Motion.countdown`,
   `Motion.pulse`, `CountdownHairline` and `UITestScenario`; consumes the shared `Countdown` and
   `AutoStopPresentation` that step 8 created first.
8. Device change, app PR (steps 7 to 11 of that plan): end reasons, the auto-stop, the armed row
   in the menu bar and the sidebar (the bubble row follows with step 7). Landed before step 7, so
   it created the shared `Countdown` and `AutoStopPresentation` in the shapes that plan specifies.
9. App icon. Independent, can land any time; listed here so the release rehearsal ships with
   it. The bubble's `BubbleGlyph` switches when it lands.
10. Redesign, layout PRs (steps 3 to 12 of that plan): display helpers over `titleOrigin`,
    seeding, window and nav column with `columnVisibility` fixed, cards, the detail pane with the
    owned header stack, onboarding look, menu bar, sheet, settings, screenshots in both
    appearances.

## Shared decisions

- Jamie is the visual reference, but only features already in scope become navigation:
  no People, Tasks, Chats or Ask AI entries.
- Light mode is the first review target; both appearances ship.
- The accent stays achromatic. A brand hue is an open question in the redesign plan and the
  icon plan, to be decided together if at all.
- Recording is one hue. The red `destructive` dot or glyph marks recording on every surface;
  every level meter (`LevelBars`, the bubble's bars) is achromatic (`strong` over `border`);
  `live` green is reserved for success. Red is a dot or a glyph, never a fill.
- The menu bar item stays; the bubble, the sidebar control and the detail header Stop are
  additional surfaces over the one `RecordingController`, never replacements.
- The auto-stop is a cancellable 90 s countdown that stops by default; "Keep recording" and
  Stop carry equal weight on every surface. The prompt's countdown shows no number; a countdown
  that ends a recording shows both the hairline and the number.
- Device changes never end a recording on their own; the session rebuilds in place and ends
  with `.deviceLost` only after four failed restarts.
- One vocabulary in user copy: "export", never "delivered".
- Accessibility ids for one action on several surfaces read `<surface>-<action>`:
  `sidebar-record`, `sidebar-stop`, `header-stop`, `bubble-stop`, `prompt-record`; the menu
  bar keeps `record-call`, `record-in-person`, `stop-recording`. Smoke tests pin ids and the
  `isSelected` trait, never copy.
- Launch arguments for the UI-testing environment are parsed once by `UITestScenario(arguments:)`.
- Every plan's `swift test` claim for Linux means the local `steno-swift` container; CI has no
  Linux job, so PR bodies state the run and its test count.

## Ownership

One plan specifies each shared thing; the others reference it.

| Thing | Owner |
|---|---|
| Record control behaviour, labels, ids, `RecordingControlPresentation`, `activeMeetingID`, `refreshPermissions`, `AppController.startRecordingFromWindow(mode:)`; `LevelBars` (achromatic), `StatusDot`, `StopLabel` in `Recording/RecordingViews.swift`; the Stop treatment (secondary button, `destructive` dot and label, elapsed time inside); list empty-state copy | start-recording plan |
| Tokens, `Theme.Radius`, components (system PR); record control geometry, fills, type and motion; detail header Stop control (`header-stop`); the header stack order (banner, title, meta, end reason, retention, progress or levels, tags, tabs); detail empty state; `displayTitle` over `titleOrigin`; per-state copy for list entry and header and for queued, processing and failed tab bodies; the one-pulse rule | redesign plan |
| `Motion.countdown`, `Motion.pulse`; the shared `Countdown` and `CountdownHairline`; `AutoStopPresentation` and its `line`; `UITestScenario`; the panel, prompt and bubble | floating indicator plan |
| Auto-stop policy (`AutoStop`, `RecordingController.autoStop`) and the armed row on every surface; `RecordingEndReason` and its sentences; `Meeting.endReason`, `Meeting.titleOrigin`, migration `v3`; `CaptureNotice`, the rebuild | device-change plan |
| Onboarding pages, rows and copy; `steno.onboardingCompleted`; setup banner and its ids; `Settings.llmConfigured` / `vaultConfigured`; `MeetingDetailViewModel.summaryStatus` and `exportStatus` and the ready-without-summary and export footer copy; `AppController.openSettings(_:)` deep links | onboarding plan |
| Retention default, the delivery guard, `RetentionSweep.keepAll()`, the Audio tab, the detail retention line, the onboarding retention sentence, `OnboardingViewModel.init(environment:)` | retention plan |
| App icon and its script; the menu bar keeps SF Symbols | icon plan |

## Review application log (2026-09-28; C = correctness, T = tests, E = elegance review)

- C1 applied (blocker): retention Decision 4, Spec item 4, guard rule 2, header line, step 3: `keepForever(assetIDs:)` sets retention and the stamp in one write; step 3 test that `redeliver` afterwards leaves `expiresAt` nil.
- C2 applied: device Decision 6, steps 2 and 4: the gap goes through `FrameRelay` on the injected clock with a 5 ms retry; `writeGap` dropped from `LaneFrameSink`.
- C3 applied: floating Decision 8, steps 3 and 5, Open questions: `openWindow` captured in `RootView` inside the `MenuBarExtra` scene and handed to `follow(_:openMain:)`; `NSWorkspace` fallback and the open question deleted.
- C4 applied: icon step 2 and Verification: fractional density via `awk`; "16 px render is 16 x 16" added.
- C5 applied: every wrong citation in the start, floating, onboarding and redesign plans replaced from the wrong-to-actual table; each Findings section states commit `9cd7cf5`; three citations spot-checked against the files.
- C6 applied, with T2's flag name: redesign step 7a: no seeded `.recording` row; `-steno-start-recording` starts the synthetic recorder after `launch()`.
- C7 applied: retention header line and step 5: `setKeepAudio(false)` stamps only when every delivery is `.delivered`; extra case in `testKeepAudioTogglesRetention`.
- C8 applied: device step 1: `"v3"` appended to `Migrations.identifiers`.
- C9 applied: start Decisions, Behaviour spec, step 4a; redesign Components table: `Button` plus `Menu` in one `HStack`, not `Menu(primaryAction:)`.
- C10 applied: redesign Decision 14 and Selection styling reworded: the highlight follows the system accent and cannot be recoloured; opaque `listRowBackground` covers it, `Color.clear` reveals it.
- C11 applied (with T19, E10): floating step 2: `BubblePresentation.make` takes the `Sendable` `AutoStopPresentation`, never the main-actor class.
- C12 applied: floating Decision 2 and step 3 name the main-actor hop and the re-registration.
- C13 applied: start "Permission gating": only the microphone can be `.denied` in the live app.
- C14 applied (with E23): index preamble names the three owner-approved additions; device step 11 adds the scope line.
- C15 applied: icon steps 1 and 3: the SVG landed with `0d6dd14`.
- C16 applied: redesign states table cites `Recording/RecordingViews.swift`; the banner is row 1 of the header stack.
- C17 applied: onboarding page 1 quotes the retention plan's wording.
- C18 applied: retention Findings quote `{"keepDays":7}`.
- C19 applied: device step 11: the pointer edit also fixes the audio plan's "Poll every 2 s" table line.
- C20 noted, no change: the `swift-ci.yml:307-362` citation was already correct; the surrounding ones were fixed under C5.
- T1 applied (blocker, with C2): device step 4: gap measured on `ManualClock`; `aDeviceChangeKeepsRecordingOnTheSameFiles` and `aGapLongerThanTheRingIsWrittenInFull` with exact values and `droppedSamples == [:]`.
- T2 applied (blocker): redesign step 7a: `-steno-start-recording`, `LaunchSmokeTests.testHeaderStopEndsTheRecording`; processing entry asserts the chip, not the stage label.
- T3 applied: redesign step 4: `preview(seed:)` keeps `.sample`; `.rich` behind `-steno-rich-seed`; `testRichSeedSpansThreeDaysAndEveryState`.
- T4 applied (with E22): start step 8 and floating step 8 count `meeting-` ids and read the `isSelected` trait; no title text anywhere in the smoke suite.
- T5 applied: device step 3: `changeDeviceAfter` fires once per instance (`changesRemaining`); `twoDeviceChangesRebuildTwice`.
- T6 applied: device tables and steps 4 and 8: the seven untested rows name their tests.
- T7 applied: retention step 6 introduces `OnboardingViewModel.init(environment:defaults:)`; onboarding step 8 extends it; `init(permissions:)` stays for the matrix.
- T8 applied: onboarding Decision 9 and step 5 (`AppController.openSettings(_:)`); start Decisions and step 4 (`startRecordingFromWindow(mode:)`), tested in `AppControllerTests`.
- T9 decided otherwise: resolved by E4's `titleOrigin` instead of the regex: `DisplayTitleTests` keeps the time-zone case (now trivially true) and adds a `.user` title identical to the old default rendering verbatim.
- T10 applied: redesign step 12: `-steno-appearance`, `-steno-window 960x600`, parameterised helper, named attachments; numeric checks stay a reviewer reading.
- T11 applied: onboarding steps 1 and 2: `PipelineHarness` optionals; `CLITests` line 113 assertions rewritten.
- T12 applied: onboarding steps 1, 6, 7 and the banner spec: invariant test, `setup-summaries` / `choose-vault` / `banner-not-now` ids, `SettingsSetupTests` (replacing `derive`, see E19), `TabText` `setup` argument.
- T13 applied: retention step 2: `FakeDestination(failUntil:)`, `aFailedDeliveryDefersExpiryUntilRedeliverSucceeds`, `rerunSummaryAlsoStampsADeferredExpiry`, `assetsRoundTripAndExpire` re-checked.
- T14 applied: retention Model additions and step 4: `.fileSizeKey`, exact assertions, runs on macOS and in the container.
- T15 applied: device step 1: `SchemaSnapshotTests` iterates every prefix; `callEnded(appName: nil)` in `payloadEnumsEncodeReadably`.
- T16 applied: device step 7: `TestSupport.deviceLosingCaptureSession` passes `environment.clock` and the test advances it.
- T17 applied in part: start step 8 queries descendants by id; the `Menu` half is moot after C9 (`sidebar-record` is a plain `Button`). Skipped: "the redesign should name the chip line", because the start plan no longer asserts the chip.
- T18 applied: floating steps 3, 7 and 8: `FloatingPanelModel` over `PanelHost` with a fake, `MenuBarLabelPresentation.make`, `isSelected` instead of frontmost, `XCTNSPredicateExpectation`, the `.stopping` resolve assertion in `testShutdownStopsTheRecordingAndTheDetector`.
- T19 applied: floating step 2 defines `AutoStopPresentation`; device step 9 fills it.
- T20 applied: redesign steps 6, 7, 8: `testEmptyTabsSayWhetherContentIsStillComing` named, `preview(seed:permissions:)`, the window opened from `AppBootstrap` under the flag, arrow test clicks first and presses once.
- T21 applied: icon step 4: `icns` magic and a 1024 representation instead of a byte threshold, either `CFBundleIcon*` key, `SOURCE.sha256`, `testMobileIconIsOpaque1024`.
- T22 applied: every "Linux container" claim now reads "local `steno-swift` container; CI has no Linux job"; `UITestScenario(arguments:)` with a unit test in the floating plan, extended by the redesign.
- E1 applied: start (new decision, `LevelBars` fill), floating Decision 6, redesign Decision 3 and tokens, index Shared decisions: red dot, achromatic meters, green for success only.
- E2 applied: redesign states table gains the `skippedUnconfigured` and `skippedRunnable` rows and the `exportStatus` footer, selectors named; onboarding Decision 8a; "export" never "delivered".
- E3 applied: redesign Decisions 4 and 7, main window, step 5: `columnVisibility` fixed to `.all`, sidebar toggle removed, two-column fallback named.
- E4 applied: device Decision 9a and step 1 carry `Meeting.titleOrigin` in `v3`; redesign Decision 13 and Non-goals read it; the "no StenoCore" non-goal bends by one column, stated.
- E5 applied: redesign split into the system PR (steps 1, 2, 13) and the layout PRs; index Order rewritten to the proposed sequence.
- E6 applied: redesign Decision 13, `MeetingCard`, `MeetingEntry`: `Date.FormatStyle` everywhere a wall-clock string is rendered; `clockText` stays for durations.
- E7 applied: redesign Decision 17 and Detail pane list the header stack; onboarding banner, device end-reason row and retention line each name their row.
- E8 applied: retention Decision 6, header line, Verification: nothing rendered for kept-forever-with-files; "Deletes on"; Reveal in Finder stays in the Actions menu.
- E9 applied: floating step 1 introduces `Countdown` and `CountdownHairline`; `DetectionPromptViewModel` holds one; device step 8's `AutoStop` uses it.
- E10 applied: `AutoStopPresentation.line` is the one owner of the countdown sentence; device Surfaces, Copy and step 9 carry the value, not a string.
- E11 applied: device Surfaces: armed bubble 64 pt, line `sm` `strong`, "Keep recording" as a 28 pt secondary button; floating Geometry table notes the height.
- E12 applied: floating Decision 6: 28 pt `raised` hairline square with a `destructive` `stop.fill` glyph, hover `card`; no red fill.
- E13 applied: floating Decision 7 reworded as the shared rule; index Shared decisions.
- E14 applied: redesign Colour tokens and step 1: `sidebar = background.over(card)` with a test.
- E15 applied: redesign tokens, type scale, entry, states table: previews and meta on `muted-foreground`; `faint` for placeholders, footnotes, step caption.
- E16 applied (14/19 variant): redesign Type scale and Tabs spec 14/19 and 13/17 as `TextSize` stores them, `lineSpacing(5)`; no fork of the mobile-mirrored value.
- E17 applied: redesign Motion paragraph and `RecordingControl` row: only the list entry dot pulses; Stop dots beside a clock are static.
- E18 applied (chip variant): redesign Decision 13 and meta row: "Monday 10:06" heading with a neutral source chip; the meta row drops date, time and source when the title is derived.
- E19 applied (app variant): onboarding Decision 8, steps 4, 6, 7: `extension Settings { llmConfigured, vaultConfigured }` and `AppController.setupBannerDismissed`; `SetupState` class and its observer gone.
- E20 applied: device Decision 7, tables, step 2: `.deviceLost` dropped from `CaptureNotice`.
- E21 applied (with C1): retention Decision 4, Model additions, step 3: `RetentionSweep.keepAll()` in core, tested in `RetentionSweepTests`.
- E22 applied (with T4): start step 8 and floating step 8 pin ids and the `isSelected` trait.
- E23 applied (with C14): index preamble.
- E24 applied: retention, floating, icon, device, redesign: engineering questions moved to "Risks and checks" beside the steps; Open questions hold owner decisions only.
- E25 applied: start plan adds `StatusDot(color:)` at 6 pt (it lands first); redesign Components table lists it; bullets 4 pt.
- E26 applied: redesign Decision 9, Spacing and radii, step 1: `Theme.Radius.xl/lg/md/sm/xs`; floating plan reads `Radius.xl` instead of adding `radiusXL`.
- E27 applied: `header-stop` in redesign Decision 15 and states table and floating Decision 11; index Shared decisions state the pattern.
- E28 applied: device copy uses two sentences; floating spec calls the 40 pt box a rounded bar at radius 16; redesign Decision 8 uses `-0.025 em`.
- E29 applied: device step 2 drops `DeviceChangeReason.synthetic` (the synthetic backend reports `.defaultInputChanged`); step 10 puts `RecordingEndReason.sentence` in `Design/Labels.swift`.
- E30 applied: redesign Non-goals: the "view models beyond pure display helpers" line deleted; step 3 says why.
