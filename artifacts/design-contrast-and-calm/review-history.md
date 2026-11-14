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
