# Design contrast and calm: council review history

Plan: `.plans/2026-11-14-design-contrast-and-calm.md`. Reviewers do not read
this file. Each round is a fresh task with the brief, the screens, that
reviewer's own earlier findings and the responses; never a target score or
the other reviewer's findings.

## 2026-11-14 — baseline audit (round 1)

Read-only tasks at high effort, request IDs `steno-design-baseline-astra-r1`
and `steno-design-baseline-fable-r1`. Evidence: 100 screens from
`pnpm screens` at the branch head (light and dark, main window at 1200×760
and 960×600, Settings, onboarding, stories), kept outside the repository.

**GPT-6-Astra: current UI 5.5/10, plan 6.5/10.** Visual only; no keyboard or
screen-reader audit. UI findings, ranked:
1. `--faint` is 2.29:1 light, 2.51:1 dark; `/80` and `/60` modifiers weaken
   more text (`theme.css:27`, `form-row.tsx:84`).
2. At 960×600 the fixed columns take 556 px and the transcript gutter 140 px;
   the setup banner and the detail header push speech to the bottom
   (`main-window.tsx:50`, `transcript-tab.tsx:111`).
3. Onboarding's denied-permission row squeezes the explanation into five
   lines beside two buttons (`form-row.tsx:76`).
4. Dark fields and unchecked boxes nearly disappear; light Settings pile up
   cards, shadows and dividers (`theme.css:112`, `card.tsx:14`).
5. Feedback is inconsistent: export failure is muted text, download failure a
   notice; failed meetings and delete use red against the live-only rule
   (`meeting-detail.tsx:304`, `meeting-list.tsx:301`).

Plan findings:
- `--outline` #8a8a8a is 2.95:1 on the proposed hover #ededed; #888888 clears
  3.03. Faint ratios in the plan are wrong. Tests must cover opacity-modified
  text, focus styles, status icons, switches, avatar tints. White on the
  destructive button is 3.76:1.
- D3 contradicts Damian's later #622 comment that save confirmations should
  be floating toasts; an inline choice must be named as a departure. Map
  errors and destructive actions to Steno's colour rules explicitly.
- Step 5 leaves the setup banner and onboarding grid unspecified; a property
  grid could add height. Require visible meeting content at 960×600; keep
  speaker identity visible in the transcript.
- Removing both `border-r` merges sidebar and list, which share `bg-sidebar`;
  give them distinct surfaces. Jamie keeps section dividers in Settings.
- #121212 is Damian's example, not a mandate; Jamie's #1e1e1e gives white
  16.67:1. Jamie's button ring is `ring-outline/50`, not `foreground/15`.
- Add interaction checks (keyboard, focus, enlarged text, announcements).
  Meeting deletion already confirms in the host; audit before adding more.

**Claude Fable 5.1: current UI 6/10, plan 6/10.** UI findings, ranked:
1. Onboarding and permission rows squeeze the description to about 15
   characters (`permission-row.tsx`); not in the plan.
2. Dark mode has no ladder (`theme.css:112-127`).
3. Result lines are untoned, the #622 pattern: export footer
   (`meeting-detail.tsx:304-310`), summaries connection error
   (`summaries-section.tsx:252-272`).
4. Non-text below 3:1: focus ring `ring-ring/24` 1.68, switch off track 1.48,
   field and checkbox borders 1.44, tab track, light status icons 2.09/2.47,
   light sidebar icons 2.27.
5. Text below AA: `--faint` 2.28/2.49 including placeholders, muted on
   accent 4.40, callout bodies at `/80` 3.31 and 4.22.
6. Noise: the "Summaries are off" card is the loudest element on every
   meeting; six-item meta line; column rules, grain, glass; a decorative
   indigo source dot.

Plan findings: `--outline` #8a8a8a fails on #ededed (2.95), use #888888 or
darker; D3 recommends the inline confirmation Damian rejected after testing
(toggles need none, results get a toast, errors inline); the test misses
painted veils (`ring-ring/24`), switch track, status and sidebar icons;
callout `/80` unnamed; delete already confirms, phone Remove does not; step 5
adds a hover reveal that does not exist today; the type scale drops the 12,
11 and 10 px tiers; light card equal to canvas leaves the ladder, say so;
onboarding and the nag card are out of scope though most visible; `--faint`
"retired or raised" left open; the lint rule has no marker to match.

### Responses (plan revised the same day)

Accepted:
- `--outline` #858585 light (3.15 on #ededed), #7a7a7a dark (3.03 on
  #313131); dark muted #a3a3a3 (5.16); light status icons on the 700s;
  destructive fill #dc2626 (white 4.83). Measured table replaced with the
  composited ratios, including the non-text pairs.
- Step 1 tests text, non-text at 3:1 and surface steps; opacity modifiers on
  colour tokens are banned outside the kit so painted veils become measured
  tokens.
- `--faint` retired; its sites and placeholders move to muted. Callout bodies
  drop `/80`.
- D3 rewritten: changes that save on change confirm nothing; results get a
  toast; errors inline as a toned row. The departure from "toasts
  everywhere" is named and reasoned.
- D4 added: failures amber, red only for live and a destructive confirm.
- Confirm gap is phone Remove; delete already confirms in the host.
- No new hover reveal: transcript times stay visible; the speaker name leads
  the paragraph instead of the 140 px gutter.
- Type scale keeps `meta` 12 and `caption` 11 with a mapping table.
- Light card equal to canvas stated as intended (hairline box).
- Onboarding row stacking and the "Summaries are off" card moved into step 3.
- Label/value header dropped for one `meta` line; acceptance: first
  transcript paragraph and summary heading visible at 960×600.
- Sidebar keeps its own surface; list and detail on canvas with one hairline
  between them; Settings sections keep a hairline under the heading.
- #121212 described as Damian's example; Jamie's graphite listed as D1's
  alternative. Jamie's ring corrected to `ring-outline/50`.
- Each PR adds a keyboard walk, a 200 % text screen and an announcement check.
- Lint marker: `role="status"` and `aria-live` only inside the kit.

## 2026-11-14 — plan round 2

Request IDs `steno-design-plan-astra-r2` and `steno-design-plan-fable-r2`.
Same screens; nothing implemented yet.

**GPT-6-Astra: plan 7.5/10.** Resolved most round 1 findings; partly:
feedback policy, avatar contrast, narrow window (column widths unchanged).
Open and new:
- Red on a destructive confirm still breaks "red is live"; use an achromatic
  confirm with the consequence in words.
- D3's premise is false for onboarding, which has an explicit Save
  (`setup-page.tsx:301-307`) and no draft persistence; give it the toast.
- #191919 on #121212 is 1.0655, failing the ≥ 1.07 assertion; #1a1a1a gives
  1.0764. Light card equal to canvas (1.00) needs an explicit exemption.
  Faint is 2.29/2.51, not 2.28/2.49.
- Person colours: light p1/p2/p3 initials 3.64/2.68/3.10 on white; the
  speaker picker keeps them; red and amber identities clash with the rules.
- The `role="status"` lint does not catch untoned feedback (the export
  `<div>` bypasses it); add targeted migration checks and require
  announcements.

**Claude Fable 5.1: plan 7/10.** Every round 1 defect addressed on paper;
partly: tokens the test promises to check have no value (`--focus`, dark
hairline, light sidebar icon, switch track, dark status). New:
- The light selected row vanishes once the list moves onto the white canvas.
- One `--focus` at 3:1 "against every surface" is infeasible (red fill,
  primary); needs an offset ring measured against the surface outside.
- The opacity ban is on the wrong side: 26 veils are inside the kit, 1
  outside; ban `/NN` everywhere and name the kit's veils.
- D4 keeps red for the destructive confirm, which is not live either; the
  destructive tokens, callout and red "Failed" badge are left untyped.
- `body` 14/22 silently replaces `text-reading` 15/1.6; `text-[9px]` and
  `text-[10px]` unmapped; button `xs` and the 30 px input unmapped.
- "Tab against its track" has no track after step 5.
- Dark status tones have no values; warning 700 is 3.5 on #191919.
- `--faint` has about 30 sites, not 20.

### Responses (plan revised the same day)

Accepted all from both reviewers:
- Dark canvas #1a1a1a (1.08 unrounded); light card and popover exempt by
  name from the step assertion.
- A value table for every token: canvas hover #f5f5f5 and selected #ededed,
  dark hairline #2e2e2e, `--focus` #68686e and #a3a3a3 drawn outside the
  control with a 2 px surface offset, sidebar icons on muted, switch track on
  `--outline`, dark status on the 400s (≥ 5.1 on #313131), live #dc2626 and
  #f87171.
- Person colours re-hued away from red and amber to blue, teal, violet and
  green 700/800 in light (≥ 5.2 on their tint) and 300s in dark (≥ 7.4); the
  test covers each on its tint.
- `/NN` banned on colour tokens everywhere; the kit's veils become named
  tokens.
- D4: failures amber, destructive confirms achromatic with the consequence
  in words, `--destructive` removed; red is live only. The departure from
  #622's red error is named.
- Onboarding's Save gets the success toast; the no-confirmation rule is
  limited to controls that save on change.
- `reading` 15/24 kept for summary and transcript; 9 and 10 px mapped;
  button `xs` kept, `lg` folded, input to h-9.
- Tab indicator measured against the canvas.
- Feedback migration enforced by component tests on the two known lines;
  the lint only reserves `role="status"` and `aria-live` for the kit.
- 960×600: if the acceptance fails, the list column narrows first.
- Faint corrected to 2.29 and 2.51, about 30 sites.

## 2026-11-14 — plan round 3

Request IDs `steno-design-plan-astra-r3` and `steno-design-plan-fable-r3`.

**GPT-6-Astra: plan 8.5/10.** Verified outlines 3.15/3.03, dark canvas step
1.0764, faint 2.29/2.51. Partly resolved: contrast coverage and opacity
enforcement. Remaining:
- Status text on composited tints still fails: warning #b45309 on its 8 %
  tint over the sidebar #f5f5f5 is 4.36, on selected #ededed 4.29; dark info
  on its 16 % tint over #313131 is 4.25. Specify passing foreground and tint
  pairs and test each tint over its permitted hosts.
- The `/NN` ban misses element opacity: the Select chevron's `opacity-50`
  gives 2.08 light, 2.62 dark. Cover element and ancestor opacity, with named
  exceptions for disabled, decorative and transitions.

**Claude Fable 5.1: plan 8/10.** Every round 1 and 2 finding resolved;
recomputed every pair in the value table. Remaining:
- Light warning #b45309 is 4.29 on #ededed and 4.09 on its tint over it (a
  selected failed meeting); use #92400e.
- Live text #dc2626 is 4.13 on #ededed; keep #b91c1c for text, #dc2626 for
  dot and fill. Dark: white on #f87171 is 2.77, the fill needs dark text.
- Per-surface values (focus offset, accent and selected on sidebar vs
  canvas) need a container-scoped mechanism; three sites hard-code
  `ring-offset-background`.
- `--destructive` also feeds the button and menu variants, `--live` and the
  recording bubble's stop glyph; say what each becomes.
- `opacity-*` escapes the ban (`disabled:opacity-64`, badge icon, pulse).
- Kit veils are about 38, not 26; `summary-tab.tsx:28` is the string, not the
  card; `text-base` (2 sites) unmapped.

### Responses (plan revised the same day; council stopped here)

Accepted all from both reviewers:
- Status text measured on its own tint over every host: light warning
  #92400e (5.49), success #065f46 (6.14), info #1d4ed8 (5.41); dark #fcd34d,
  #6ee7b7, #93c5fd (≥ 5.65).
- Live split into text #b91c1c (5.53 on #ededed) and dot and fill #dc2626;
  dark fill #f87171 carries #1a1a1a text (6.29).
- Container-scoped `--surface`, `--accent` and `--row-selected`; focus offset
  `ring-offset-(--surface)`; the test iterates surfaces.
- Every `--destructive` user mapped: button and menu variants achromatic,
  `--live` its own token, bubble stop glyph on `--live`.
- `opacity-*` on text and icons banned, with disabled, `aria-hidden` and
  transition exceptions; Select chevron and badge icon fixed.
- Setup notice reference corrected to `meeting-detail.tsx:93-104`;
  `text-base` mapped; veil count corrected.

Final scores: Astra 8.5, Fable 8. Remaining findings were value and
mechanism corrections, all applied; no structural objection open. Handed to
Nicolai for D1–D5 rather than a fourth round.

## 2026-11-14 — Nicolai's decisions

- D1: #121212, as in June and vamoo; Damian's rules win over Jamie's.
- D2: bundle a font of Steno's own; which one is open (Geist, DM Sans or
  Hanken Grotesk rendered as candidates; Instrument Sans dropped).
- D3, D5: at the recommendation.
- D4: errors are red; amber is too weak for a failure and reads as a
  warning. Plan reverted from amber failures; destructive confirms red as in
  June. Error text #b91c1c light (5.01 on its tint over #ededed), #fca5a5
  dark (6.00 on its tint over #313131).

## 2026-11-15 — built milestone 1 (round 1 on built UI)

Request IDs `steno-built-m1-astra-r1`, `steno-built-m1-fable-r1`. Evidence:
`pnpm screens` before and after each of design steps 1-3 (#354, #355, #356),
the landing page (#351) and the diff of speaker step 1 (#352).

**GPT-6-Astra: visual 7/10, accessibility and UX detail 6/10.** Ranked:
1. #352 advances after an uncommitted pick, even when Combine is cancelled
   (`speaker-name-field.tsx:143` calls `finish(true)` at once;
   `selectSpeaker()` does not distinguish cancel from success); errors still
   reach only the console. Return committed/cancelled, await, inline error.
2. Failures still look like warnings: amber errors, or an amber icon beside
   grey text (settings general error, summaries failed, export failed).
   Step 6: red tinted result rows; amber for permission and setup.
3. Onboarding stacking adds scrolling: Local network falls below the
   viewport, the Allowed badge stacks (`form-row.tsx:78`). Keep status-only
   rows horizontal.
4. Dark record mark on the white Stop button is 2.77, was 3.76
   (`record-mark.tsx:20`); measure a live colour for the primary fill.
5. 960x600 transcript still cramped by the 140 px gutter (step 5).
6. Naming still hides evidence; the avatar previews before commit (speaker
   step 2).
Lessons: blanket stacking too indiscriminate; the contrast matrix misses live
marks on primary fills; keyboard, 200 % text and announcement checks not
done; design captures still show the old "87% match" fixture (fixed only on
#352's branch); the Call dot turned green through the person palette.
#351: inherited eyebrow `--fg-dim` #71717a on #09090b 4.12
(`apps/site/src/app/globals.css:39`).

**Claude Fable 5.1: visual 6.5/10, accessibility and UX detail 7/10** (before
about 5 and 4). Ranked:
1. Regression: the source dot uses person colours (`source.tsx:13-15`); p4
   remapped to green, so "Call" carries a green or lime dot. Make it neutral.
2. `--dashed-outline` (24 % foreground, `theme.css:166`) is 1.62 light, 2.12
   dark and unmeasured; use `--outline`, hover `--focus`, add to the test.
3. Light warning #92400e reads brown, 1.10 from the error red; split icon and
   text tokens, raise the light tint toward 16 %, re-measure.
4. Focused fields draw three lines (`input.tsx:17` border plus ring plus
   offset); tabs use another focus style (`tabs.tsx:55`).
5. Meta line wraps with a dangling dot at 960 (`meeting-detail.tsx:632`).
6. "Deletes Oct 29" twice (meta line and footer); keep the footer.
7. Failures red in the list, amber in the detail; should not ship split.
8. Active tab as an outlined button beside an outlined Template select;
   Template wraps under the tabs at 960.
9. "Set up · Not now" equal links; Not now should be a muted button.
10. Lone "Copy transcript" row; fold into the ⋯ menu.
11. Story sheet: dark "Sidebar card" story unreadable (story scoping).
Plan lessons: dark sidebar-to-canvas step 1.08 reads as one surface once the
hairline goes (expect #1c1c1c-#1d1d1d); no keyboard walk, 200 % or
announcement checks in any PR; no dialog in the screens. Unbuilt steps:
cut meta items and move Template for 960; fix warning before reds; rebase
screens on #352. #351 nothing wrong (mono not preloaded, unchanged).
#352 correct; Confirm path still logs to the console; merge target picked by
label string.

### Responses

All accepted. Fixes go on top of the stack before milestone 2; failures turn
red now rather than in step 6; the keyboard walk, a 200 % screen and an
announcement check are added as captured screens.

## 2026-11-15 — built milestone 1, round 2 (after the fixes on #356, #352, #351)

Request IDs `steno-built-m1-astra-r2`, `steno-built-m1-fable-r2`. Evidence:
`design-fix1/after/` (156 screens incl. keyboard focus walk, 200 % zoom,
dialog).

**GPT-6-Astra: visual 7.5/10, accessibility and UX 6.5/10.** Resolved: #352
outcomes, record mark on primary, neutral Call dot, #351 dim text 5.22.
Partly: failures red but several are only a red icon beside neutral text;
onboarding granted fits, denied/unknown still scroll. Remaining:
1. HIGH: 200 % zoom breaks the layout (Export over the breadcrumb, clipped
   title and pill, tabs offscreen; the two columns take 440 of 600 px).
2. Enter can still create an unseen person when options lag
   (`speaker-name-field.tsx:229`); speaker step 2.
3. Export-failed and connection-failed rows neutral and not announced
   (`screens.spec.ts:492` exemption); step 6.
4. Naming hides evidence; screens still show the old fixture (not rebased on
   #352).
5. 960 transcript gutter; step 5.
6. Onboarding denied state cuts Calendar's actions, Local network below the
   fold.
Responses: 1, 3 and 6 sent to the design steps 4-6 build as requirements
(narrow-window reflow with a real zoom assertion; ResultLines; onboarding
spacing). 2 and 4 belong to speaker step 2.

**Claude Fable 5.1: visual 7.5/10, accessibility and UX 7.5/10** (round 1:
6.5 and 7). Resolved: source dot, dashed outline, warning hue, one focus
ring, meta line at 960, Deletes once, failures red, Set up vs Not now, dark
stories, dark step 1.11, evidence captures, #352 outcomes. Partly: meta line
breaks inside the date at 200 %; pill tabs with ring plus inset outline.
Not: lone Copy transcript row. Ranked:
1. Header actions overlap the breadcrumb at 200 % and, with a held
   recording, at 960 ("Meetings / P"; `header.tsx:63`,
   `content-column.tsx:29`).
2. Meta line wraps inside "Tuesday, Sep 29 · 14:50" (`meeting-detail.tsx:651`).
3. Speakers pill clips "2 need names" at 200 % (`speakers-popover.tsx:80`).
4. Dark disabled primary is a bright grey block.
5. Callout bodies differ by tone (`callout.tsx:27-35, 54`).
6. The grey source dot now encodes nothing.
7. Dialog body has no edge under the header when scrolled.
8. Stale "Summaries are off" story.
9. Step 6 reminders (export-failed tone in the icon only; two rows not
   announced).
Responses: all sent to the design steps 4-6 build as requirements.

## 2026-11-15 — built milestone 2 (design steps 4-6: #357, #358, #359)

Request IDs `steno-built-m2-astra-r1`, `steno-built-m2-fable-r1`. Evidence:
`design-{4,5,6}/after/`.

**GPT-6-Astra: visual 8.5/10, accessibility and UX 7.5/10.** Resolved: 960
transcript gutter, onboarding denied fits, 200 % view no longer overlaps,
underline tabs, focus rings. Partly: failure feedback (export red and live;
processing failure neutral and unannounced). Remaining:
1. HIGH: the narrow-layout drawer hides recording status, the auto-stop
   countdown, Keep recording and recorder errors; reading another meeting
   hides Stop (`main-window.tsx:127`, `sidebar.tsx:268`,
   `meeting-detail.tsx:463`). Keep them outside the drawer.
2. Processing failure neutral, unannounced (`meeting-detail.tsx:521`); "Try
   again" reruns the summary for a transcription failure.
3. Drawer has no visible Close; restore focus to Show meetings.
4. Settings summaries: the connection result sits below the viewport; put
   Connection before Advanced.
For speaker step 2 (sent to that build): evidence kept while naming, Enter
on visible highlight only, panel fits at 200 % incl. height, stable rows,
focus on Escape, advance after ack, announced Retry, long names in 400 px.

**Claude Fable 5.1: visual 8.5/10, accessibility and UX 8/10.** All its
earlier items resolved (crumbs, meta line, pill at 200 %, disabled primary,
callout bodies, source dot, dialog edge, stale story, underline tabs, Copy
in the ⋯ menu). Ranked:
1. List titles misaligned by the state dot (`meeting-list.tsx:321-342`).
2. ⋯ menu: radio items without a leading slot, Re-run summary read as a
   third template (`menu.tsx:173-189`, `meeting-detail.tsx:598-627`).
3. Evidence gap: the summaries-failed ResultLine is cut off in the capture.
4. "Summaries are off" line without tone icon; Not now reads as prose.
5. Two filled primaries on onboarding page 1.
6. Dialog edge a few px below the clip top.
7. Failed meeting: Process again and Try again duplicate.
8. "Meetings" three times on one screen.
9. Keyboard walk lost the template stop.
For speaker step 2 (sent to that build): a stable play slot for voices
without a clip, picker anchored below the name, no "87% match", roving
tabindex across transcript names, announced rename with focus back, one
identity signal (avatar before the transcript name), the count-only pill
opens the panel and is not trapped by the drawer.

### Responses

All accepted: design findings from both reviewers go to one fix build on
#359 (council round 3 there); speaker items to the speaker step 2 build,
which rebases onto the fixed design stack before finishing.
