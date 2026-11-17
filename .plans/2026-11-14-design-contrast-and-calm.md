# Design: measured contrast, a computed dark palette and Jamie's calm
**Status:** D1 to D5 decided 2026-11-14; building 2026-11-15

Line references are against main at `cd7ab805f` (after #346 and #347). The
speaker work in #348 and its follow-up live in
`.plans/2026-11-14-speaker-identification.md`; where #348 changes a file
cited here (`meeting-detail.tsx`, `transcript-tab.tsx`, `button.tsx`), the
step that touches it rebases on #348 first.

The web UI (`apps/macos/web/`) looks busier than Jamie's, and none of its
colours are measured. This plan applies two sources to it, in this order of
precedence:

1. **Damian's guidance** from vamoo (`NicolaiSchmid/vamoo`), as June applied
   it (`NicolaiSchmid/june` `.plans/2026-11-04-token-contrast.md` and
   `.plans/2026-11-05-design-craft.md`):
   - vamoo #1286 (dark mode): not an inversion; a very dark grey base, not
     pure black (Material's #121212 is his example); elevation is a lighter
     surface, since shadows do not show on dark; every text tier and every
     icon passes WCAG AA on every surface it can sit on, not only the base;
     white on the base at least 15.8:1; status tones get their own dark
     versions; contrast checked in CI.
   - vamoo #622 (feedback): each result shows its tone in a tinted background
     and a tone icon, not only a thin border or a dot; text AA in both modes;
     errors stay inline where they happened; after testing, save
     confirmations as floating toasts, because inline "Saved" text went
     unnoticed (Damian, 2026-10-20).
   - June's follow-through: one contrast module and a test that reads the
     stylesheet; a surface ladder that steps (vamoo 1.07 and 1.17); a 1 px line
     on resting things and a shadow only on floating ones; control outlines
     and focus indicators at 3:1 (WCAG 1.4.11); destructive actions confirm
     and name the consequence; nothing reachable only on hover.
2. **Jamie's calm** (app.meetjamie.ai bundle of 2026-11-14), wherever it does
   not contradict 1: no ornament on controls, surfaces instead of rules, a
   stepped type scale, flat settings, fewer items per list row.

Steno's own rules stay: the primary is achromatic and nothing loads from the
network. The colour rule changes with this plan (D4): red is for what is live
and for errors and destructive actions, amber for what needs the user but has
not failed.

## Measured on 2026-11-14

WCAG relative luminance, alpha veils composited source-over onto the surface
they are painted on. Checked by hand and by both reviewers before the module
exists.

| Pair | Light | Dark | Needs |
|---|---|---|---|
| `--faint` (about 30 sites, every placeholder) on canvas | 2.29 | 2.51 | 4.5 |
| muted text on `--accent` (hover, selected) | 4.40 | 4.73 | 4.5 |
| callout warning body at `/80` on its tint (`callout.tsx:27,35`) | 3.31 | 6.47 | 4.5 |
| callout destructive body at `/80` | 4.22 | ok | 4.5 |
| field focus ring `ring-ring/24` (`input.tsx:19`) | 1.68 | 2.06 | 3 |
| switch off track on card (`switch.tsx:20`) | 1.48 | 1.22 | 3 |
| `--input` field and checkbox outline | 1.44 | 1.19 | 3 |
| warning and success 500 icons on canvas (`status-icon.tsx:17-18`) | 2.09, 2.47 | ok | 3 |
| `--sidebar-icon` (`sidebar-row.tsx:16`) | 2.27 | 3.44 | 3 |
| person initials p1, p2, p3 on white | 3.64, 2.68, 3.10 | | 4.5 |
| white on the destructive button | 3.76 | | 4.5 |
| active tab against its track (`tabs.tsx:28,55`) | 1.18 | 1.06 | visible |
| sidebar against canvas | 1.02 | | ≥ 1.07 |
| dark card against canvas; dark sidebar is #000000 | | 1.05 | lighter step, not black |

Jamie fails several of the same rules: muted #72727a is 4.37 on its own
sidebar #f5f5f5, inputs sit on the 1.20:1 hairline, the button focus ring is
`ring-outline/50`. Its values are not copied; its structure is.

## Where Jamie and Damian disagree, Damian wins

| Jamie | Here |
|---|---|
| Muted text #72727a | Computed per surface to pass 4.5 on the worst one |
| Inputs and checkboxes on the hairline | A separate `--outline` at 3:1; the hairline stays for dividers |
| Faint `ring-outline/50` focus | A `--focus` ring at 3:1 against the surface around the control |
| Dialogs without a shadow | Light: floating things keep a shadow. Dark: they lift a step |
| Timestamps and actions on hover | Stay visible; nothing new moves behind hover |
| Graphite dark canvas #1e1e1e | #121212 base with lighter steps (D1). Jamie's also passes 15.8:1; the choice is consistency with June and vamoo |

## Steps

Each step is one pull request with `pnpm screens` before and after (light and
dark, 1200×760 and 960×600), a keyboard walk through every changed control
with the focus ring visible on each, one screen at 200 % text size, and a
check that toasts and result lines are announced. Each waits for Nicolai's OK.

1. **Contrast module, test and the opacity ban.**
   - `src/theme/contrast.ts`: WCAG maths, source-over compositing of alpha
     and `color-mix()`, hex and `rgb()`, checked against published reference
     pairs first.
   - `src/theme/contrast.test.ts` parses `src/theme.css` and asserts, for
     `:root` and `.dark`:
     - text at 4.5 on every surface it may sit on: canvas, sidebar, card,
       popover, accent, row hover, row selected, each status tint, each person
       tint, the primary fill;
     - non-text at 3: `--outline` and the switch off track on the same
       surfaces; status icons and sidebar icons; `--focus` against every
       surface a control sits on (the ring is drawn outside the control with a
       2 px offset in the surface colour, so the control's own fill is not the
       background it is measured on); the tab indicator against the canvas;
     - steps: sidebar to canvas and canvas to each raised surface at least
       1.07, compared unrounded. Exempt by name: light card and popover, which
       equal the canvas and carry a hairline instead.
   - Opacity at call sites is what makes today's tokens pass on paper and fail
     on screen. `scripts/check-ui-restyle.mjs` rejects `/NN` on every colour
     token everywhere, the kit included; the kit's colour veils become named
     tokens in `theme.css` (about 38), so the test sees every colour that is
     painted. The same rule rejects `opacity-*` on an element or an ancestor
     of text and icons, with named exceptions: `disabled:` states (exempt
     from WCAG), `aria-hidden` decoration, and transitions that end at full
     opacity. The Select chevron's `opacity-50` (2.08 light, 2.62 dark) and
     the badge's `[&_svg]:opacity-80` go.
   - Per-surface values come from the container, not the call site. Each
     surface (sidebar, canvas, card, popover) sets `--surface`, `--accent`
     and `--row-selected` for its subtree; the focus offset is
     `ring-offset-(--surface)`, replacing the three hard-coded
     `ring-offset-background`. The test iterates the surfaces and their
     scoped values, so it knows what a control sits on.
   - Status and live text are measured on their own tint composited over
     every host it may appear on (sidebar, canvas, row hover, row selected,
     card), not only on bare surfaces.
   - Pairs that fail at this step are listed by name; the test fails once one
     clears, so the list only shrinks and is empty after step 2.
2. **Computed palette.** `src/theme/palette.ts` derives the dark ladder from
   #121212 as steps toward white (no brand tint: the primary is achromatic)
   and the dark status tones from the Tailwind 400s; the test fails when
   `theme.css` drifts from it. Every value is verified against its worst
   surface:

   | Token | Light | Dark |
   |---|---|---|
   | sidebar | #f5f5f5 | #121212 |
   | canvas (`--background`) | #ffffff (1.09) | #1a1a1a (+3.5 %, 1.08) |
   | card, popover | #ffffff, hairline box | #232323 (1.11) |
   | accent, row hover | on sidebar #ededed; on canvas #f5f5f5 | #2a2a2a |
   | row selected | on sidebar #ffffff; on canvas #ededed (1.17) | #313131 |
   | hairline (dividers only) | #eaeaeb | #2e2e2e |
   | `--outline` (fields, checkboxes, switch off track) | #858585 (3.15 on #ededed) | #7a7a7a (3.03 on #313131) |
   | `--focus` | #68686e (4.73 on #ededed) | #a3a3a3 (5.16 on #313131) |
   | muted text, sidebar icons | #68686e (4.73 on #ededed) | #a3a3a3 (5.16 on #313131) |
   | warning text and icons | #92400e (5.49 on its 8 % tint over #ededed) | #fcd34d (7.90 on its tint over #313131) |
   | success text and icons | #065f46 (6.14 on its tint over #ededed) | #6ee7b7 (6.55 on its tint over #313131) |
   | info text and icons | #1d4ed8 (5.41 on its tint over #ededed) | #93c5fd (5.65 on its tint over #313131) |
   | person colours on their 16 % tint | #1d4ed8, #115e59, #6d28d9, #166534 (≥ 5.2) | #93c5fd, #5eead4, #c4b5fd, #bef264 (≥ 7.4) |
   | live and error text (`--live-foreground`, `--destructive-foreground`) | #b91c1c (5.01 on its 8 % tint over #ededed) | #fca5a5 (6.00 on its tint over #313131) |
   | live dot, error and destructive fill (`--live`, `--destructive`) | #dc2626, white text 4.83 | #f87171, text #1a1a1a 6.29 |

   White is 18.7 on the dark base and 17.4 on the dark canvas. Person colours lose red and amber, which
   belong to live and needs-the-user; they stay hues so speakers remain
   distinguishable. `--faint` is retired: its sites, every placeholder among
   them, move to muted text; anything fainter is decorative and
   `aria-hidden`. Callout bodies use the tone's text colour at full opacity.
   The canvas colour also lives in `WebCanvas`
   (`apps/macos/Steno/Web/WebWindowView.swift:134`), pinned by
   `ThemeTokensTests`; both move here. The Tauri shell pins none.
3. **Calm kit and the two loudest defects.**
   - Remove the control shadow and edge highlight (`button.tsx`, `input.tsx`,
     `checkbox.tsx`), the grain, the press scale and the glass on menus and
     dialogs. Popups are solid: a hairline ring, `rounded-[12px]`, the
     floating shadow in light, a lighter surface in dark. Radii: controls 8,
     surfaces 12, dialog 18 to 12.
   - Sizes: button `xs` h-6 (list rows, badges) stays; `sm` h-8 px-3;
     default h-9 px-4; `lg` folds into default. Input 30 px to h-9.
   - Onboarding and permission rows (`permission-row.tsx`, the grid in
     `form-row.tsx:76`) stack their buttons under the description when the
     row is narrower than about 520 px, so the explanation keeps its width.
   - The setup notice at the top of the reading column ("Summaries are
     off.", `meeting-detail.tsx:93-104`) stops being the loudest thing on
     every meeting: one muted line with a "Set up" link in the summary tab,
     and nothing on the transcript tab. One-time notices (`isNotice`) keep
     their callout.
4. **Type scale.** Tokens in `@theme`, every current size mapped:

   | Token | Size/line, weight, tracking | Replaces |
   |---|---|---|
   | `display` | 22/30, 600, −0.25 | page and meeting titles |
   | `headline` | 18/26, 600 | settings section titles |
   | `reading` | 15/24 | summary and transcript (`text-reading`, unchanged size) |
   | `title` | 14/20, 500, −0.15 | row titles, labels |
   | `body` | 14/22 | descriptions, settings text |
   | `label` | 13/20, 500 | sidebar rows, buttons |
   | `meta` | 12/16 | list metadata (`text-xs`) |
   | `caption` | 11/14, 500 | badges, counters (`text-2xs`, `text-3xs`, `text-[10px]`) |

   The two `text-base` sites map to `headline` (summary headings,
   `summary-tab.tsx:143`) and `title` (the medium empty state).

   `text-[9px]` is decorative only and becomes `caption` or `aria-hidden`.
   Hierarchy comes from size, weight and the muted colour, never opacity.
5. **Layout calm**, inside today's three columns:
   - the sidebar keeps its own surface; the meeting list and the detail sit
     on the canvas, with no rule between sidebar and list and one hairline
     between list and detail; the list's selected row uses the canvas
     selected colour from step 2;
   - meeting rows: state dot and title, then one `meta` line with time and
     preview; source line, avatars, duration and the "No summary" badge go;
   - underline tabs with a foreground indicator, measured against the canvas;
   - the detail header: title, then one `meta` line with date, length and
     speakers; tags stay where they are. No label/value grid, because it
     costs height at 960×600;
   - the transcript drops the 140 px gutter: the speaker's name in bold leads
     the paragraph, its time stays visible beside it in `meta`;
   - flat Settings: page title, headed sections separated by a hairline under
     the heading (as Jamie does), rows without cards, a 720 px column;
   - acceptance: at 960×600 with the default column widths (sidebar 256,
     list 300, `column-widths.ts`) the first transcript paragraph and the
     first summary heading are visible without scrolling. The resizable
     columns (#347) already narrow the list first to keep the reading column
     at 400 px; this step changes the heights above the reading, not the
     widths.
6. **Feedback and confirmations.**
   - Controls that save on change (Settings toggles, selects, fields) confirm
     nothing: the control's state is the confirmation.
   - Explicit commits report success in a floating toast with a tone icon, as
     Damian asked: onboarding's Save (`setup-page.tsx:301-307`), export,
     pairing, the summaries connection test, "Process again". Errors stay
     inline beside the control as a toned row: tinted background, tone icon,
     text at AA. New kit components `toast.tsx` and `result-line.tsx`; both
     own their `aria-live` region, so every result is announced.
   - Migration with tests, not a lint promise: the export footer
     (`meeting-detail.tsx:304-310`) and the summaries connection error
     (`summaries-section.tsx:252-272`) move onto `ResultLine`, and their
     component tests assert the tone and the live region. The lint rule bans
     `role="status"` and `aria-live` outside the kit, so a new announced
     result can only come from it; a reviewer checks new unannounced
     feedback in the pull request.
   - Destructive confirms name the consequence in words ("Remove iPhone: it
     can no longer send recordings") on a red confirm button, as June does. Phone Remove (`iphone-section.tsx:183-195`) revokes on one
     click today and gets one. Meeting deletion already confirms in the host
     (`delete-meeting.ts:4-5`).
   - Errors are red, as in #622: a failed meeting, a failed export or
     connection, the "Failed" badge and filter, and the destructive callout
     use the error tone (`--destructive`, text and tint from step 2).
     Amber stays for what needs the user without having failed (permissions,
     setup). `--live` (`theme.css:54-55`) gets its own token from step 2 so
     the two can drift apart later; today both are red. The button
     `destructive` variant (`button.tsx:35-38`), the menu item variant
     ("Delete meeting", `menu.tsx:77`) and the recording bubble's stop glyph
     (`recording-bubble.tsx:158`) keep their red.
   - The decorative indigo source dot on the summary loses its hue.

## Decisions for Nicolai

- **D1: dark base.** Decided: #121212 with the lifts in step 2, as in June
  and vamoo. Where Jamie and Damian differ, Damian's rule wins.
- **D2: font.** Decided: Geist and Geist Mono (OFL) on every Steno
  surface, as vamoo's dashboard has them. The phone app already loads Geist
  (`mobile/src/App.tsx`; Geist Mono joins it). The web UI bundles the woff2
  files (400, 500, 600 and the mono 400, 500) from the `geist` package
  beside the app, with `font-display: swap` and the system stack as
  fallback; `scripts/check-offline.mjs` keeps proving nothing loads from
  the network and `scripts/check-bundle.mjs` gets the new budget. The
  landing page swaps its self-hosted DM Sans and JetBrains Mono
  (`apps/site/src/app/globals.css`) for the same files, and the landing
  page rule in `AGENTS.md` changes with it. The type scale's tracking is
  set for Geist. The font lands in step 3 with the calm kit, the landing
  page in its own pull request.
- **D3: confirmations.** Decided at the recommendation: as in step 6, toasts for explicit
  commits and none for changes that save on change. Damian's "toasts
  everywhere" was written for vamoo's save buttons; Steno has one, in
  onboarding, and it gets the toast.
- **D4: failure colour.** Decided: errors are red, as Damian has them
  (#622); amber reads as a warning and is not strong enough for a failure.
  Red now means live, error or destructive; amber means needs the user.
  The rule changes where it is written, the comment at
  `apps/macos/web/src/theme.css:48`. The landing page keeps its own rule
  (red for the live state only), since it shows no errors.
- **D5: order.** Decided at the recommendation: steps 1 to 3 first, since they fix the
  measured failures and the two loudest defects; 4 to 6 after Nicolai has
  seen them.

## Review

The council June used: GPT-6-Astra and Claude Fable 5.1, independent
read-only reviews at high effort. Each round is a fresh task with the
brief, the screens, that reviewer's own earlier findings and the responses;
never the target score or the other reviewer's findings. The history lives
in `artifacts/design-contrast-and-calm/review-history.md`, which reviewers
do not read. Atlas runs no full typecheck or test suite; CI does.

## Out of scope

- Two-pane routing or a collapsible sidebar (Jamie's layout): a plan of its
  own.
- The landing page (`apps/site/`) and the phone app beyond the font (D2), and
  the native panels.
- Jamie's purple, calendar colour bars and tag colour dots.
