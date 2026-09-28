# Steno: first-run feedback, program note

Status: proposal, 2026-09-28. Index for the seven plans triggered by the owner's first run of
the macOS app (two rounds of feedback on 2026-09-28). Binding context: [`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md)
and [`2026-09-25-macos-app-and-release.md`](2026-09-25-macos-app-and-release.md). Nothing here
widens the scope; every plan below restyles, surfaces or wires what v1 already has.

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

1. Start recording from the main window. Smallest change, unblocks the owner's testing;
   supplies the recorder seams every later plan reads.
2. Onboarding, core part (PR 1 of that plan): stop running the fake cleaner and summariser in
   the product so the next test recording produces honest output.
3. Audio retention: default to keep forever for new installs, the delivery guard, and the
   visible setting. The owner flips the stored 30-day row once.
4. Onboarding, app part (PRs 2 and 3 of that plan): status rows, banner, the second
   onboarding page.
5. Floating recording indicator and detection prompt. Depends on step 1; ships before the
   redesign because the owner cannot see recording state today. Adds `radiusXL`,
   `Motion.countdown` and `Motion.pulse`.
6. Device changes during a recording. Correctness work in StenoAudio; its app part adds the
   auto-stop row to the surfaces from steps 1 and 5.
7. App icon. Independent, can land any time; listed here so the release rehearsal ships with
   it.
8. Visual redesign, in its own step order. Last because it touches every screen and the
   plans above change copy and controls it restyles.

## Shared decisions

- Jamie is the visual reference, but only features already in scope become navigation:
  no People, Tasks, Chats or Ask AI entries.
- Light mode is the first review target; both appearances ship.
- The accent stays achromatic. A brand hue is an open question in the redesign plan and the
  icon plan, to be decided together if at all.
- The menu bar item stays; the bubble, the sidebar control and the detail header Stop are
  additional surfaces over the one `RecordingController`, never replacements.

## Ownership

One plan specifies each shared thing; the others reference it.

| Thing | Owner |
|---|---|
| Record control behaviour, labels, ids, `RecordingControlPresentation`, `activeMeetingID`, `refreshPermissions`, `LevelBars` and `StopLabel` in `Recording/RecordingViews.swift`, the Stop treatment (secondary button, `destructive` dot and label, elapsed time inside), list empty-state copy | start-recording plan |
| Record control geometry, fills, type and motion; detail header Stop control (`stop-recording-header`); detail empty state; every window token except the three below; per-state copy for list entry, header and tab bodies | redesign plan |
| `radiusXL`, `Motion.countdown`, `Motion.pulse`; the panel, prompt and bubble | floating indicator plan |
| Auto-stop countdown, its copy and the armed row on every surface (bubble, menu bar, sidebar); `RecordingEndReason` | device-change plan |
| Onboarding pages, rows and copy; `steno.onboardingCompleted`; setup banner and detail status rows; Settings deep links | onboarding plan |
| Retention default, the Audio tab, the detail "Recording" line, the onboarding retention sentence | retention plan |
| App icon and its script; the menu bar keeps SF Symbols | icon plan |
