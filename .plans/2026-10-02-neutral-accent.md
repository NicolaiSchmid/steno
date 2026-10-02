# Proposal: drop the green, go neutral like T3 Code

Status: option A implemented 2026-10-02. Refines `2026-10-01-t3code-design-polish.md`
("Deviations we keep on purpose: green primary instead of blue") and
`2026-09-29-macos-webview-ui.md` Decision 2. Supersedes the
"green primary" deviation listed there; that file now points here. Nothing here changes layout,
type or spacing; it is colour only.

## Why the green feels off

The web UI uses one hue, `#3f8a4c` (an olive forest green, oklch chroma
≈ 0.10), for four different jobs at once:

| Job | Where | Count |
|---|---|---|
| Brand accent / call to action | Record, Stop, "Set up summaries", "Record call" in detail | every screen |
| Control state | Switch on, Checkbox checked, focus ring, progress fill | every Settings row |
| "Live" semantics | pulsing dot on the recording row, Stop button, auto-stop callout, iPhone connected dot | recording state |
| "Success" semantics | "Allowed" badge, delivered export icon, success Callout (uses emerald `#10b981`, a second green) | Settings, onboarding |

Three things make the result look dated rather than clean:

1. **The hue is muddy.** T3 Code's accent is a saturated blue (oklch 0.488
   0.217 264, chroma 0.22). Our green sits at half that chroma, next to the
   emerald "Allowed" badge which is a different green again. Two greens on
   one screen and neither of them crisp.
2. **Green means "go", not "recording".** The Stop button, the live dot and
   the auto-stop warning are all green. Every other recorder on the Mac
   (QuickTime, Voice Memos, Zoom, the system orange mic dot) says red.
3. **The people palette is earth-toned.** Avatars are solid rust, ochre,
   sage and slate discs (`--p1…--p4`) with white initials. Together with
   the olive and the amber "Confirm speakers" button the whole page reads
   autumn. T3 Code has no coloured discs anywhere; its only tints are the
   8 % status surfaces.

The Mac web UI is also the odd one out in our own product: `mobile/global.css`
and `apps/macos/Steno/Design/Theme.swift` already use an **achromatic**
primary (`#171717` light / `#ffffff` dark, with a `live` token for red).

## Three options (rendered with `pnpm screens`, 2026-10-02)

Mockups in `/tmp/steno-compare/*.png` (current, A, B, C side by side). All
three share the changes in "Common changes" below; they differ only in
`--primary`.

### A. Neutral primary, red for live (recommended)

| Token | Light | Dark |
|---|---|---|
| `--primary` | `#171717` | `#ffffff` |
| `--primary-2` | `#262626` | `#e5e5e5` |
| `--primary-foreground` | `#ffffff` | `#0a0a0a` |
| `--primary-soft` | `rgba(0 0 0 / 8%)` | `rgba(255 255 255 / 12%)` |
| `--ring` | stays `var(--primary)` | |

Record is a black button in light and a white button in dark, with the bars
mark in the foreground colour; while recording, the mark turns red and
pulses. Switches, checkboxes and the progress bar follow (black/white on),
which is the Vercel and Linear-settings idiom and matches the phone app.
The page has exactly one chromatic signal at a time: red when something is
live, amber when something needs you, red text when something failed.

Why this over B: it is what the phone app and the native menu bar popover
already do, so the three surfaces finally agree; it keeps us from being a
blue T3 Code clone; and it gives the red recording state room to be the
only strong colour on screen.

### B. T3 Code's blue

| Token | Light | Dark |
|---|---|---|
| `--primary` | `oklch(0.488 0.217 264)` | `oklch(0.571 0.21 264)` |
| `--primary-2` | `oklch(0.53 0.217 264)` | `oklch(0.61 0.21 264)` |
| `--primary-soft` | `rgba(27 78 216 / 12%)` | `rgba(52 107 241 / 20%)` |

Pixel-identical to T3 Code's accent. Crisp, familiar, and the switches look
like macOS's own. Costs: a third hue on the recording screen (blue Stop,
red mark, amber Confirm), and we lose the only thing that made the window
ours.

### C. Keep green, make it clean

| Token | Light | Dark |
|---|---|---|
| `--primary` | emerald-600 `#059669` | emerald-500 `#10b981` |
| `--primary-2` | `#10b981` | `#34d399` |

Smallest change: one saturated Tailwind green instead of the olive, and it
collapses the two greens into one (`--success` becomes an alias). It still
leaves the Stop button green and still competes with the success badge.
Only worth it if the green is a brand decision we want to keep.

## Common changes (all options)

1. **Live is red.** Add `--live: var(--destructive)` (fill) and
   `--live-foreground: var(--destructive-foreground)` (text) rather than
   reusing destructive at call sites, so the two can diverge later.
   - `meeting-list.tsx` `TONE.recording` → `text-live-foreground`; the pulse
     dot → `bg-live`.
   - `RecordMark` gets `text-live` while `pulse` is on (sidebar Stop, header
     Stop, the recording bubble's web mirror if any).
   - `Callout` `live` variant → the `default` look (card surface, hairline
     border) with the icon in `text-live`. A countdown is a notice, not a
     tinted banner.
   - `StatusIcon` `tone="primary"` → `tone="success"` for "delivered"
     (`meeting-detail.tsx:231`); drop the `primary` tone.
2. **The iPhone dot uses the success token**: `bg-success` with a
   `--success-surface` ring (`sidebar.tsx:311`), not `--primary-2`.
3. **Soft avatars.** `Avatar` tones become 16 % tints with the hue as text:
   `bg-p1/16 text-p1` and so on; drop `text-primary-foreground` from the
   base. People palette moves to Tailwind 600 (light) / 400 (dark):

   | | Light | Dark |
   |---|---|---|
   | `--p1` | rose `#e11d48` | `#fb7185` |
   | `--p2` | amber `#d97706` | `#fbbf24` |
   | `--p3` | emerald `#059669` | `#34d399` |
   | `--p4` | indigo `#4f46e5` | `#818cf8` |

   The source dots in `source.tsx` (`before:bg-p2…p4`) follow automatically.
   Transcript speaker labels (inline speakers, WP0-WP8) use the same tokens
   and get the same lift.
4. **Success stays emerald** for badges and callouts; nothing else is green.
5. **Native surfaces need nothing.** The recording bubble and the native
   status chips already draw "recording" in `stenoDestructive`; the Swift
   `live` token is only used for success states ("Ready", a passed check),
   so `Theme.swift` and the phone app stay as they are.
6. **Docs.** Update the "Deviations we keep on purpose" line in
   `2026-10-01-t3code-design-polish.md` and the `WebCanvas` comment if the
   dark primary changes what the host paints.

## Files

- `apps/macos/web/src/theme.css` (tokens, `@theme inline` entries for
  `live`, `live-foreground`)
- `apps/macos/web/src/components/ui/{avatar,callout,record-mark,status-icon}.tsx`
- `apps/macos/web/src/windows/main/{sidebar,meeting-list,meeting-detail}.tsx`
- `apps/macos/web/src/stories/stories-page.tsx` (the stories already show
  the live callout and the pulsing mark)
- `apps/macos/web/e2e` screens are the review evidence; no new tests beyond
  the existing token and component tests that assert class names.

## Out of scope

Layout, type, spacing, the amber warning idiom, the three-column layout,
the Record split button. If option A is chosen, the `warning-outline`
"Confirm speakers" button stays amber; it is the one "needs you" signal.
