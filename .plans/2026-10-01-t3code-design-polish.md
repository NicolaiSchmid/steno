# Design polish: match T3 Code (pingdotgg/t3code)

Status: implemented 2026-10-01 (PR #148). Follows `2026-09-29-macos-webview-ui.md` Decision 2
("T3 Code's recipe on Steno's layout") and refines it against the current upstream.

Reference: `pingdotgg/t3code` at `5cc99e1c` (origin/main, 2026-10-01), local
checkout `/tmp/t3code`. Files read: `apps/web/src/index.css`, `components/ui/*`,
`components/ui/sidebar.tsx`, `components/Sidebar.tsx`, `components/sidebar/*`,
`components/WorkspacePageHeader.tsx`, `components/settings/settingsLayout.tsx`,
`SettingsGroup.tsx`, `packages/shared/src/themePalettes.ts`,
`apps/desktop/src/window/DesktopWindow.ts`, plus the marketing screenshot
`apps/marketing/src/assets/app-desktop.webp`.

Steno side: `apps/macos/web/src/theme.css`, `components/ui/*`, `windows/*`, rendered
with `pnpm screens` (note: Linux renders DejaVu Sans, so the screenshots look larger
and heavier than on a Mac; judge sizes from the CSS, not the PNGs).

## What already matches

Light palette (bg `#fcfcfc`, sidebar `#fafafa`, card white, fg zinc-800, border
zinc-200, muted fg zinc-500), `--radius` 10 with the sm/md/lg/xl/2xl ladder,
`--control-radius` 8, glass blur/opacity/saturation, menu and popover shadow
`0 16px 40px -18px / 55%` (dark `0 18px 44px -18px / 80%`), menu item radius 6 and
separator `mx-2 my-1`, primary button inset highlight and `active:scale-[0.97]`,
system font stack, `ui-monospace` for times, grain on sidebar and canvas,
`--ease-drawer`, Base UI + cva + the no-restyle lint rule.

## Deviations we keep on purpose

The green primary was dropped on 2026-10-02 for an achromatic one with red for
the live state; see `2026-10-02-neutral-accent.md`.

Three columns; the Record split button at the top of
the sidebar; no theme engine, contrast slider or font settings; no wordmark.

## 1. Type scale (the biggest visible difference)

T3 Code never uses 13 px. Root is 16 px; primary UI text is `text-sm` (14/20),
secondary is `text-xs` (12/16), dense metadata is `text-2xs` (11/16) and counters
are `text-3xs` (10/14). Steno sets `body { font: 13px/1.45 }` and then uses 13,
13.5, 12.5, 11.5 ad hoc.

| Role | Steno today | T3 Code |
|---|---|---|
| Row titles, menu items, buttons, labels | 13 | 14 `text-sm font-medium` |
| Secondary lines, timestamps, descriptions | 12 / 12.5 | 12 `text-xs` |
| Dense metadata, small badges | 11 / 11.5 | 11 `text-2xs` |
| Counters, jump hints | 10 | 10 `text-3xs` |
| Dialog and empty-state titles | 15 semibold | 20 `text-xl font-semibold leading-none` |
| Settings section heading | 16 semibold + description | 14 `text-sm font-normal text-foreground/70`, no description |
| Settings row title / description | 13 / 12 | 14 medium / 12 `text-muted-foreground/80` |

Changes:
- `theme.css`: drop the 13 px body font; set `html { font-size: 16px }`,
  `body { font-family: var(--font-sans); -webkit-font-smoothing: antialiased }`
  and add `text-2xs`/`text-3xs` to `@theme inline` with the line heights above.
- Replace every `text-[13px]`, `text-[13.5px]`, `text-[12.5px]`, `text-[11.5px]`
  with `text-sm`, `text-xs` or `text-2xs`.
- Weights stay: medium for controls and rows, semibold for headings only.
- Add `[text-box:trim-both_cap_alphabetic]` on text that sits beside an icon in
  the header and breadcrumb (T3 uses it on the breadcrumb and wordmark).

## 2. Tokens

| Token | Steno | T3 Code | Change |
|---|---|---|---|
| `--input` (light) | `#e4e4e7` | zinc-300 `#d4d4d8` | darken field borders one step |
| dark `--sidebar` | `#121212` | `#000` | sidebar darker than the canvas, not lighter |
| dark `--muted-foreground` | `#a1a1aa` | ≈ `#818181` (neutral-500 90% + white) | darken; sidebar muted stays `#a3a3a3` |
| dark `--card`/`--popover` | `#121212` | `#111111` (bg + 3% white) | close enough, align to `#111` |
| dark sidebar row hover / active / selected | 4% / 5% / 5% | fg 8% / 11% / 7% | raise |
| dark sidebar border | 6% | white 8% | add `--sidebar-border` |
| dark `--primary` | same as light | lifted (oklch L 0.488 → 0.571) | lift the green in dark, e.g. `#4c9a5a` → `#5aae69`; keep text contrast on white |
| status colours | warning + destructive | `error`, `warning`, `success`, `info`, each with `-foreground` (700 light / 400 dark) and `-surface` (8% light / 16% dark) | add `success` and `info`; use `*-foreground` for text and the 500 only for fills and icons |
| destructive text | `#dc2626` everywhere | text `red-700`/`red-400`, fill `red-500` | split into fill and foreground |
| `--faint` | `#a1a1aa` / `#6b6b74` | none; uses `muted-foreground/40…/60` and `secondary-label` | keep, but define it as `color-mix(muted-foreground 60%, background)` so it tracks the theme |
| disabled opacity | 50% | 64% | `disabled:opacity-64` everywhere |
| focus ring | `ring-2 ring-primary/50 offset-1` | `ring-2 ring-ring ring-offset-1 ring-offset-background` (full primary) | full-strength ring; fields use `border-ring` + `ring-[3px] ring-ring/24` |
| field shadow | `0 1px rgb(0 0 0/4%)` | `shadow-xs/5` + `before:` 1 px highlight (light: `0 1px black/4%` below, dark: `0 -1px white/6%` above) and `not-dark:bg-clip-padding` | apply the `before:` idiom to inputs, selects, checkbox, popover, tooltip, outline buttons |
| primary button shadow | `inset 0 1px white/16%, shadow-xs` | same plus `shadow-primary/24`; active `inset 0 1px black/8%`; disabled/active `shadow-none` | add the tinted shadow and pressed state |
| dialog glass | `popover 92%` + `shadow-pop` | `background 80%`, border fg 10%, shadow `0 24px 64px -24px / 65%`; dark border white 8%, `inset 0 1px white/4%, 0 24px 72px -20px / 90%` | own shadow for dialogs |
| dropdown glass bg | `popover 80%` | `color-mix(popover 18%, color-mix(popover 80%, transparent))` (more opaque) | adopt |
| scrollbar | ScrollArea 8 px thumb `foreground/25` | 6 px, thumb `rgb(217 217 217)` / white 8%, hover `rgb(191 191 191)` / 12%, radius 3, fades in on hover with 300 ms delay | adopt the tokens and width, add global `::-webkit-scrollbar` for native scroll containers |
| grain | 160 px tile, 2 octaves, `multiply`/`screen` blend | 256 px tile, 4 octaves, opacity 0.035, no blend mode, also on `body` | optional; T3's reads slightly finer |

## 2a. Light mode, explicitly

Light is the default appearance and most of the light-mode drift is in how
little contrast T3 uses between states. Values from `index.css:1027-1095` and
`[data-app-sidebar]` (`:1141-1183`):

| Surface or state | T3 Code light | Steno light today | Change |
|---|---|---|---|
| Canvas `--background` | zinc-25 `oklch(99.2% 0 0)` ≈ `#fcfcfc` | `#fcfcfc` | keep |
| Sidebar `--sidebar` | zinc-50 `#fafafa` | `#fafafa` | keep |
| Card, popover | white | white | keep |
| `--secondary`, `--muted` | zinc-50 `#fafafa` | muted `#fafafa` | add `--secondary` |
| `--accent` (menu highlight, ghost hover) | zinc-100 `#f4f4f5` | `#f4f4f5` | keep |
| Sidebar row hover | zinc-25 `#fcfcfc` (one step lighter than the sidebar, barely visible) | `--accent` `#f4f4f5` (darker than the sidebar) | hover must go *lighter*: `--sidebar-row-hover: #fcfcfc` |
| Sidebar row active / selected | white, no ring | white + `shadow-xs` + 1 px border ring | drop the ring and shadow |
| List row active | white (`--sidebar-row-active`) | white card with border and shadow | white fill only |
| Outline button at rest | `bg-popover` (white) on `border-input` zinc-300, `shadow-xs/5`, 1 px `black/4%` bottom highlight | white on `#e4e4e7`, `0 1px black/4%` | border zinc-300 `#d4d4d8` |
| Outline button hover | `bg-accent/50` (half of zinc-100) | `bg-accent` | halve it |
| Input / select at rest | `bg-background` `#fcfcfc` on zinc-300, `shadow-xs/5` | `bg-card` white on `#e4e4e7` | background-tinted field, darker border |
| Input focus | `border-ring` + `ring-[3px] ring-ring/24` | `border-primary/60` + `ring-2 ring-primary/25` | 3 px ring at 24% |
| Settings group card | `bg-card/40` (40% white over `#fcfcfc`), `border-border/60`, `shadow-xs/5`, `rounded-xl` | white, full border, `shadow-xs`, `rounded-lg` | lighter border and fill, 14 px radius |
| Settings row divider | `border-border/50` | `divide-border` | halve |
| Segmented control | container `bg-input/40` (zinc-300 at 40%), pressed item `bg-background` + `shadow-xs/10` | container `bg-accent`, pressed `bg-card` + ring | adopt |
| Badge tints | `bg-{warning,error,success,info}/8` with `*-foreground` (700) text | `warning-surface` at 12% with `#b45309` | 8% fills, 700-weight text |
| Alert surfaces | warning `border-warning/32 bg-warning-surface` (8%), info `border-info/32 bg-info/4` | card + 28 px icon well at 12% | adopt |
| Kbd | `bg-muted` zinc-50, no border | `bg-muted` + `border-border` | drop the border |
| Scrollbar thumb | `rgb(217 217 217)`, hover `rgb(191 191 191)` | `foreground/25` | adopt |
| Tooltip | opaque white, `border`, `shadow-md/5` | glass | opaque in light too |
| Disabled | `opacity-64` | `opacity-50` | 64 |
| Muted text | zinc-500 `#71717a`; faint text is `muted-foreground/40`–`/60` | `#71717a`; `--faint` `#a1a1aa` | keep `--faint` ≈ `muted/60` |
| Destructive text | `red-700` `#b91c1c` (fill `red-500`) | `#dc2626` | 700 for text |

Rule of thumb for light mode: surfaces step by one Tailwind shade (25 → 50 → 100),
borders carry the separation, and hover is lighter than rest on the sidebar and
half-strength accent everywhere else.

## 2b. Global spacing reference

Every number below is from T3 Code and is what Steno should adopt. 1 unit = 4 px.

**Chrome**

| Element | T3 Code |
|---|---|
| Header row (every column) | `h-[52px]`, `items-center gap-3`, `pl/pr` 12 px (20 px at `sm`), drag region, no bottom border on content pages |
| Breadcrumb | `gap-2 sm:gap-3`, `text-sm font-medium`, items with `text-box: trim-both cap alphabetic` |
| Header actions | `size="sm"` (28 px) or `icon-sm`, right aligned, overflow `EllipsisIcon size-4` |
| Sidebar header row | `h-[52px] px-3 gap-2`; icon buttons 28 px (`size-7`) |
| Sidebar group | `p-2` (`--sidebar-content-inset` 8 px); menu `gap-1`; thread list `gap-px` |
| Sidebar footer | `px-2 py-1 gap-2` |
| Column divider | `border-l border-border`, 1 px, no shadow |
| Content column | `mx-auto max-w-[48rem]`; settings `max-w-4xl px-6 pt-6 pb-12 gap-8` |

**Rows**

| Row | Height | Padding | Inner rhythm |
|---|---|---|---|
| Sidebar menu button | 32 (`h-8`) | `px-2.5 py-1.5` | `gap-2` icon to label, icon 16 |
| Sidebar section header | 32 (`h-8`) | `mx-0.5 px-2` | label, `gap-2`, hairline, chevron 12 |
| List card row | 78 (`h-[4.875rem]`), `li py-0.5` | `px-2.5 py-2` | line 1 `h-5 gap-1.5`; line 2 `mt-1`; line 3 `mt-0.5 gap-1.5` |
| List slim row | 36 (`h-9`) | `px-2.5` | `gap-2.5`, icon 16 |
| Menu item | 28 (`min-h-7`) | `px-2 py-1` | `gap-2`, icon 16, popup `p-1`, separator `mx-2 my-1`, group label `px-2 py-1.5` |
| Select item | 28 (`min-h-7`) | `px-2 py-1` | check 14 right |
| Settings row | ≥ 44 (`py-3` + 20 px title line) | `px-4 py-3` | two-column grid `gap-8`; title line `min-h-5 gap-1.5`; description below title |
| Settings section | heading `min-h-7 px-4`, `space-y-2.5` to the card | | sections `gap-8` |
| Right-panel tab | 24 (`h-6`) | `pl-1.5 pr-2` | `gap-0.5`, icon 12; bar `h-[52px] gap-1 pl-2` |

**Controls**

| Control | Height (desktop) | Horizontal padding | Gap |
|---|---|---|---|
| Button xs / sm / default / lg | 24 / 28 / 32 / 36 | 7 / 9 / 11 / 13 px (`--spacing(n) - 1px`) | 4 / 6 / 8 / 8 |
| Icon button xs / sm / default | 24 / 28 / 32 | 0 | icon 14 / 16 / 16 |
| Input default / sm / lg | 30 / 26 / 34 | 11 px | |
| Select trigger default / sm / xs | 32 / 28 / 24 | 11 / 9 / 7 px | chevron `-me-1` |
| Badge default / sm / control | 18 / 16 / 24 | 3 px | 4 |
| Switch default / sm | 18×30 / 16×26 | 2 px | |
| Kbd | 20 | 4 px | |

**Overlays**

| Surface | Padding | Offset |
|---|---|---|
| Menu / select popup | `p-1` | `sideOffset 4` |
| Popover | `py-4`, 16 px inline (`compact`: `py-2`, 12 px) | `sideOffset 4` |
| Tooltip | `px-2 py-1` | `sideOffset 4` |
| Dialog | header `p-6 pb-3 gap-2`; body `p-6 space-y-4`; footer `px-6 py-4 gap-2` | viewport `p-4`, centred |
| Toast | `pl-3.5 pr-3.5 py-3`, stack gap 12 | top 52 + 32 px, right 32 px |
| Empty state | `gap-6 p-12`, header `max-w-sm` | |
| Alert | `px-3.5 py-3` | |

## 3. Geometry and controls (`components/ui`)

Desktop sizes (T3's `sm:` values; the unprefixed ones are touch sizes):

| Component | T3 Code | Steno today |
|---|---|---|
| Button | xs 24, sm 28, default 32, lg 36; icon-xs 24, icon-sm 28, icon 32; `text-sm`; x-padding `--spacing(n) - 1px`; svg `-mx-0.5` | sm 28, md 32, lg 36, icon 32; 13 px |
| Button variants | default, secondary, outline (bg-popover, hover `bg-accent/50`, dark `bg-input/32` → `/64`), ghost (fg text, muted icon via `--control-icon-color`), ghost-muted, ghost-destructive, destructive, destructive-outline, warning-outline, glass (`border-border/60 shadow-sm`, hover `border-border`), link | primary, outline, ghost (= T3 ghost-muted), glass, destructive |
| Input | wrapper `rounded-lg` (10), `bg-background`, inner 30 px (`h-7.5`), sm 26, lg 34, x-pad 11; `font="mono"` adds `font-mono tabular-nums` | `rounded-control` (8), `bg-card`, 32 / 28 |
| Select trigger | `rounded-lg`, default 32, sm 28, xs 24, `min-w-36`; chevron `size-3 opacity-50 -me-1`; item 28 px `rounded-sm px-2 py-1 text-sm`, selected `bg-foreground/[0.08]`; `sideOffset 4` | 8 px radius, chevron 14, item 30, `sideOffset 6` |
| Menu | item 28 px `rounded-sm px-2 py-1 text-sm`, icons 16 at `opacity-80 text-muted-foreground`, destructive uses `destructive-foreground`; shortcut is plain `<kbd>` `ms-auto font-medium font-sans text-xs tracking-widest text-secondary-label`; popup `min-w-40`, `sideOffset 4`, no scale animation | item 30 px, 13 px, Kbd chip, `min-w-[220px]`, `sideOffset 6`, scale 0.98 |
| Popover | `w-64/80/96`, `py-4` with 16 px inline padding, title `font-semibold text-sm leading-none`, `before:` highlight, `sideOffset 4` | `w-72 p-3.5`, title medium 13 |
| Tooltip | **opaque**: `border bg-popover shadow-md/5 rounded-md text-xs px-2 py-1`, `max-w-80`, `sideOffset 4`; glass is an opt-in variant; shortcuts inline as "(⌘B)" | glass, radius 7, 12 px, Kbd |
| Dialog | `rounded-2xl`, widths `max-w-md` (most), `max-w-lg`, `max-w-xl`; header `p-6 pb-3 gap-2`, title `text-xl font-semibold leading-none`, description `text-sm text-muted-foreground`; body `p-6 space-y-4` in a ScrollArea with fade; footer `border-t bg-muted/72 px-6 py-4` rounded to `2xl - 1px`; close = ghost icon `XIcon` at `end-2 top-2`; 200 ms ease-in-out scale 0.98 | `w-[420px] p-5`, title 15, footer plain |
| Badge | 18 px `h-4.5`, `text-xs`, `rounded-sm`; sizes sm 16 (10 px text), control 24; tints `bg-{x}/8 text-{x}-foreground dark:bg-{x}/16`; `outline` = `border-input bg-background` | 20 px, 11 px, grey default |
| Switch | track 18×30, thumb 14 `bg-background`, unchecked `bg-input`, 200 ms, press `scale-x-110`; sm 16×26 | 20×32, thumb 16 `bg-card` |
| Checkbox | 16 px, `rounded-[.25rem]`, `border-input bg-background`, dark unchecked `bg-input/32`, check stroke 3 at 12 px | 16 px radius 5, border 1.5 |
| Kbd | `h-5 min-w-5 rounded bg-muted px-1 font-sans text-xs font-medium text-muted-foreground`, no border | 18 px bordered mono 11 px |
| Segmented control | container `rounded-lg bg-input/40 p-0.5 gap-0.5`; item `h-6 rounded-md px-2.5 text-xs font-medium text-muted-foreground`; pressed `bg-background shadow-xs/10` (dark `bg-input/72`); count `text-3xs font-semibold tabular-nums` | Tabs: `bg-accent p-[3px]`, item 28 px 13 px, active `bg-card` + ring |
| Alert (Callout) | `rounded-xl border px-3.5 py-3 text-sm`; warning `border-warning/32 bg-warning-surface text-warning-foreground`, info `border-info/32 bg-info/4`, error `border-error/32 bg-error-surface`; icon inline, tinted, no icon well; `glass` surface for floating alerts; sidebar variant `rounded-lg border-sidebar-border bg-sidebar-control-surface px-2 py-1.5 text-[11px]` | Card with a 28 px icon well |
| Empty | icon tile `size-9 rounded-md border bg-card shadow-sm/5` with two ghost copies behind rotated ±10° at 84%; title `text-xl font-semibold`; description `text-sm`; `gap-6 p-12`; index page wraps it in `rounded-3xl border-border/55 bg-card/20 px-8 py-12` | 36 px filled well, 15 / 13 |
| Separator | `bg-border h-px` / `w-px` | none |
| Toast | top-right, `dropdown-glass rounded-lg shadow-xl shadow-black/25`, max 360 wide, offset 52 + 32 px, Sonner-style stack, corner `size-6` dismiss orb, actions `size="xs"` | none |
| Spinner / Skeleton | `LoaderCircleIcon` sizes 12–20 with `visible-animate-spin`; skeleton `bg-muted-foreground/15` stepped 2.4 s | none |

Icons: T3 uses Lucide at the default stroke 2 (no override) and `-mx-0.5` inside
buttons; 14 px is the most common size (row meta, breadcrumb), 16 for nav and
buttons, 12 for tiny affordances; muted icons come from `--control-icon-color`
(ghost/outline) or `opacity-80` (menus). Steno overrides to `stroke-[1.75]` in 11
places: drop that.

Motion: popovers and tooltips scale 0.98 on open/close; menus do not animate;
dialogs 200 ms `ease-in-out`; switch thumb 150 ms with the stretch; `status-pulse`
(2 s, `steps(6)`) and `status-ping` (2 s, `steps(8)`) are the duty-cycled live
indicators the plan asked for and Steno never added. Add `.no-transitions`
during appearance changes.

## 4. Shell: chrome and columns

- **Topbar.** Every column has a 52 px header row with `-webkit-app-region: drag`
  (children reset, controls `no-drag`). The main header
  (`WorkspacePageHeader`) is `h-[52px] items-center gap-3` with 12 px gutters
  (20 px at `sm`), holds a breadcrumb (`text-sm font-medium`, parents
  `text-muted-foreground`, `/` in `text-icon-muted`, current `text-foreground`) and
  right-aligned actions as `outline`/`ghost` `sm` buttons with an `EllipsisIcon`
  overflow. No border on content pages. Steno: give the detail column a real
  header ("Meetings / Produktstrategie 90/10", Export, …) instead of floating
  glass pills, and the list column a header row instead of the 15 px "Meetings"
  h1.
- **Traffic lights.** T3 inset is 90 px (`trafficLightPosition {x:16, y:19}`, a 28 px
  sidebar toggle right after). Steno leaves the native default; keep, but give the
  sidebar header row the same 52 px drag region.
- **Scroll fade.** `topbar-scroll-fade` masks the first 1.5 rem of scrolled content
  under the header and keeps a 6 px opaque scrollbar lane. Apply to detail and
  settings scroll containers.
- **Sidebar width.** 256 px default, resizable 208 … (viewport − 640), 16 px rail
  with a 2 px hover line, double-click resets, width persisted, offcanvas
  collapse with a header toggle. Steno is fixed at 236. At minimum go to 256;
  resizable is a nice-to-have.
- **Content width.** Chat column is `mx-auto max-w-[48rem]`; settings
  `mx-auto max-w-4xl px-6`. Steno's detail is left-aligned `max-w-[720px] px-10`:
  centre it with `mx-auto max-w-3xl`.
- **Dividers.** Plain `border-l border-border` between columns, no shadows. Same.

## 5. Sidebar

- Rows: `SidebarMenuButton` default is 32 px, `rounded-[var(--control-radius)]`,
  `px-2.5 py-1.5 text-sm font-medium`, rest colour
  `text-sidebar-muted-foreground/80`, icon 16 in `--sidebar-icon-color`
  (`color-mix(sidebar-muted-foreground 60%, sidebar)`), hover `bg-sidebar-row-hover`
  with full-strength text and icon, active `bg-sidebar-row-selected font-medium`
  with **no shadow and no border ring**. Steno's `SidebarRow` active state adds
  `shadow-xs + 0 0 0 1px border`: remove it, raise rows to 32 px / 14 px.
- Group padding 8 px, menu `gap-1`; Steno `px-2 gap-0.5` is close.
- Section headers: `h-8 px-2 text-xs font-medium text-sidebar-muted-foreground/60`,
  label, then a hairline `h-px flex-1 bg-sidebar-border/60`, then a `size-3`
  chevron (collapsible). Steno `SectionLabel` is 11 px faint with tracking: switch.
- Search: a borderless `h-8 rounded-md px-2 text-sm font-medium
  text-sidebar-muted-foreground` row with a 16 px `SearchIcon`, hover
  `bg-sidebar-row-hover`; the Kbd hint is the borderless chip. Steno's bordered
  `SearchInput` with ⌘F lives in the list column: either move search to the top
  of the sidebar in this style, or restyle the field to `rounded-lg bg-background`
  with the T3 input idiom.
- Footer: 32 px icon buttons with tooltips plus a status pill. Map Steno's phone
  card to T3's sidebar alert variant (`rounded-lg border-sidebar-border
  bg-sidebar-control-surface px-2 py-1.5 text-[11px]`), keep the Settings row.
- Dark mode: sidebar `#000`, foreground `#f1f3f7`, accent `#191a1d`, muted
  `#0a0a0a`, muted-foreground `#a3a3a3`, border white 8%, input white 18%.

## 6. Meeting list rows (T3 thread rows)

T3 "card" row: fixed 78 px, `px-2.5 py-2`, `rounded-md`, rows separated by 1 px
(`gap-px`), never a border or shadow. Line 1 (`h-5`, `text-xs`): 16 px favicon,
project name `text-secondary-label font-medium truncate`, `ml-auto` status or
timestamp `tabular-nums text-secondary-label`. Line 2 (`mt-1`): title `text-sm
font-medium truncate`, `text-foreground` when unread else `text-foreground/90`.
Line 3 (`mt-0.5`, `text-xs text-secondary-label`): 14 px icons, branch, PR badge,
`font-mono` diff stat. Active row `bg-sidebar-row-active` (white light / fg 11%
dark); hover `bg-sidebar-row-hover`. Settled rows become 36 px "slim" rows with
`text-secondary-label/70` and a `grayscale opacity-40` icon until hover. Status
text colours: working `sky-600/400`, needs input `indigo-600/300`, failed
`red-700/300`, done `emerald-700/300`, approval `warning-foreground`.

Steno row: `rounded-lg border px-[11px] pt-[9px] pb-2.5`, active `border-border
bg-card shadow-xs`, 13 px title with a mono time on the same line, 2-line
preview, then Badge chips (source, duration, state) and avatars.

Changes:
- Drop the border and shadow; `rounded-md`, `px-2.5 py-2`, `gap-px`, active = white
  (light) / fg 11% (dark), hover zinc-25 / 8%.
- Line 1: source icon 16 px + source label `text-xs text-secondary-label
  font-medium`, `ml-auto` time `text-xs tabular-nums` (sans, not mono).
- Line 2: title `text-sm font-medium`. Line 3: one-line preview or the meta
  (duration, state as coloured text with a 14 px icon, avatars `ml-auto`). Replace
  the Badge chips with icon + text; keep a badge only for a hard state ("Failed").
- Day headers in the T3 section-header style (`text-xs font-medium
  text-sidebar-muted-foreground/60` with the hairline).

## 7. Meeting detail

- Header per section 4; drop the glass pills (T3 keeps `glass` for floating
  overlays only). Title stays in the body; `text-2xl font-semibold
  tracking-tight` (24) is closer to T3's hero sizes than 26.
- Tags: `Badge variant="outline"` (`rounded-sm`, 18 px, `border-input
  bg-background`) instead of `rounded-full` pills. "Confirm speakers":
  `warning-outline` `xs` button. "Add tag": `ghost-muted` `xs` with `PlusIcon`.
- Tabs: the segmented control from section 3 (`h-6`, `text-xs`), counts
  `text-3xs font-semibold tabular-nums`. Template picker: `Select size="xs"`.
- Setup banner: Alert warning variant, inline icon, actions `size="xs"`.
- Reading text can stay 15/1.6 (T3's chat prose is user-sized). Summary h3:
  `text-base font-semibold`, margin `1.25rem 0 0.5rem`; inline code 12 px with
  `0.1rem 0.35rem` padding and radius 6; blockquote 2 px left border.
- Footer rows: `text-sm`, switch 18×30, buttons `sm`/`xs`.
- Live indicator: `status-pulse` on the Record mark and the "Live" badge.

## 8. Settings

- Nav: same sidebar (256, resizable), 32 px menu rows `text-sm font-medium`,
  icons 16, no subtitles, search row with a `/` Kbd. Steno's 44 px stacked rows
  with 11 px subtitles go.
- Header: 52 px breadcrumb "Settings / General".
- Container: `mx-auto max-w-4xl px-6 pt-6 pb-12`, sections `gap-8`.
- Section: `space-y-2.5`; h2 `min-h-7 px-4 text-sm font-normal text-foreground/70`;
  no section description.
- Group: `rounded-xl border border-border/60 bg-card/40 shadow-xs/5`; rows divided
  by `border-t border-border/50`; row `px-4 py-3 grid
  grid-cols-[minmax(0,1fr)_minmax(10rem,auto)] items-center gap-8`; title
  `text-sm font-medium` in a `min-h-5` line; description `text-xs leading-normal
  text-muted-foreground/80 max-w-xl`; status `text-xs text-muted-foreground`.
- Controls: `size="sm"` (`Select` `w-40`), header actions `xs`, inline reset
  `icon-micro` with `Undo2Icon size-3`.
- Window: 760×520 fixed cannot hold a 256 px nav plus 896 px content. Propose
  960×640 resizable with a 760×520 minimum (plan Decision 1 change).

## 9. Onboarding

- Title `text-2xl font-semibold`, intro `text-sm text-muted-foreground`, caption
  `text-xs`. Permission rows as a settings group (section 8). Footer: `border-t
  bg-muted/72 px-6 py-4` like a dialog footer, buttons `default` size.

## Work packages

1. **Tokens and type** (`theme.css`, `@theme inline`): root 16, text-2xs/3xs,
   dark sidebar/muted/primary, status roles, scrollbar, dialog shadow, focus ring,
   `no-transitions`, keyframes. Update `WebCanvas` only if `--background` changes
   (it does not).
2. **`components/ui`**: resize and restyle per section 3; add Separator, Toast,
   Spinner, Skeleton, segmented ToggleGroup, Alert; drop the stroke override.
   Re-record the stories screen.
3. **Shell**: header rows with drag region and breadcrumb, scroll fade, 256 px
   sidebar (resizable optional), sidebar rows and section headers, list rows,
   centred detail.
4. **Settings and onboarding**: layout per sections 8–9; window size change in
   `SettingsWindow.swift` and the UI-test sizer.
5. **Verify** on a Mac: `pnpm screens` on Forge or a local build; compare against
   `/tmp/t3-app-desktop.png` (the T3 marketing screenshot at 2×).

## Deviations (implementation)

- Separator, Toast, Spinner, Skeleton and the segmented ToggleGroup were not
  added: nothing calls them yet. Tabs stayed Tabs and Callout stayed Callout
  (no Alert rename).
- The meeting list column sits on the sidebar surface (`bg-sidebar`, the
  sidebar hairline and grain) rather than the canvas, so the white selected
  row and the row hover read in light mode.
- The Settings purpose sentence stays as a muted intro line under the
  breadcrumb; the XCUI tests anchor on it (`section-purpose-<id>`).
- The Settings window shipped at 960×640, resizable, as proposed.
- The sidebar stays fixed at 256 px; no resizer.
