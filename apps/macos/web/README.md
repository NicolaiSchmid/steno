# Steno web UI

The pixels of the Mac windows: one React app, bundled by Vite and served by
the app from its own URL scheme. Swift keeps the state and the audio; the page
draws snapshots and sends commands over the bridge (`src/bridge/`). Plan:
`.plans/2026-09-29-macos-webview-ui.md`. Spec for the look:
`docs/design/webview-mockup/steno-main.html`.

## Commands

| Command | What it does |
|---|---|
| `pnpm install` | pnpm 11, Node 24. Own lockfile. |
| `pnpm dev` | Vite dev server on 5173 with the fixture bridge. `#/shell`, `#/stories`, `#/shell?dark&tab=transcript&menu`. |
| `pnpm check` | `lint`, `lint:ui`, `typecheck`, `test`. Must pass before a PR. |
| `pnpm build` | Writes `dist/` with relative asset URLs. |
| `pnpm screens` | Builds, then Playwright renders the shell and the stories in light and dark to `screens/` and asserts the page made no network request. Run `pnpm exec playwright install chromium` once. |

## Two rules

**Offline.** The bundle runs from the app with `connect-src 'none'`. No web
fonts, no CDN, no fetch. The system font stack only; icons from
`lucide-react`; the grain is an inline SVG. `grep -rE "https?://" dist` finds
nothing but the SVG namespace.

**No restyling.** `src/components/ui` owns every look. Callers pick a
`variant` or `size` and may add layout classes only (`w-full`, `ml-auto`,
`min-h-0`); colour, type, radius, shadow and padding never appear at a call
site. `pnpm lint:ui` (`scripts/check-ui-restyle.mjs`) fails the build when
they do. Tokens live in `src/theme.css`; motion in `src/lib/motion.ts`.
