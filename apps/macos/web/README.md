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
| `pnpm dev` | Vite dev server on 5173 with the fixture bridge. `#/main` (the default), `#/settings?section=general\|recording\|transcription\|summaries\|export\|iphone`, `#/stories`. Flags: `dark`, `tab=summary\|transcript\|tasks\|notes`, `menu`, `picker`, `scenario=…` (below), for example `#/main?dark&tab=transcript&picker` or `#/settings?section=iphone&scenario=pairing`. |
| `pnpm check` | `lint`, `lint:ui`, `typecheck`, `test`. Must pass before a PR. |
| `pnpm build` | Writes `dist/` with relative asset URLs, then `scripts/check-offline.mjs` greps it for fetchable URLs. |
| `pnpm screens` | Builds, then Playwright renders the main window in every state at 960 by 600 and 1200 by 760, the Settings window's sections and states at its fixed 760 by 520, and the stories, light and dark, to `screens/` and asserts the page made no network request. Run `pnpm exec playwright install chromium` once. |

## Layout

| Path | What lives there |
|---|---|
| `src/bridge/` | The contract (`contract.ts`), the typed client, the WebKit and mock transports, and `hooks.ts` (`useSnapshot`, `useBridge`, `send`). |
| `src/windows/main/` | The main window over the bridge: sidebar, meeting list, detail with its tabs, `format.ts` for every date and duration. |
| `src/windows/settings/` | The Settings window: a sidebar of the six sections with the subtitles their snapshots carry, one `*-section.tsx` per section built from `FormCard` and `FormRow`, `settings-format.ts` for sizes and relative times. Text fields keep a draft (`use-draft.ts`) and send one `update` plus a `save` when focus leaves. |
| `src/components/ui/` | The component set; the only place a look is defined. |
| `src/stories/` | Every component in every variant, rendered by the screens. |
| `fixtures/bridge/` | Snapshots and replies recorded by the Swift side; the mock transport serves them. |

The mock transport answers a `?scenario=` in the page's query by bending the
fixtures (`applyScenario` in `src/bridge/mock-transport.ts`): `empty` clears
the list, `recording` makes `recording.live` the recording, `failed` selects
the failed meeting, `processing` adds and selects a meeting in the progress
entry. `?tab=` picks the detail tab. For Settings: `settings-error` (an error
with its details, a login item awaiting approval, an update available),
`download-failed`, `summaries-connected`, `summaries-failed`, `codex-consent`,
`codex` (from `settings.summaries.codex`), `export-on`, `pairing` (from
`settings.iphone.pairing`) and `phone-unavailable`. Interactive elements carry the
`data-testid` the SwiftUI views exposed as accessibility identifiers
(`sidebar-record`, `nav-all`, `meeting-<uuid>`, `tab-summary`,
`speaker-picker-<uuid>`, `processing-card`, `empty-detail-title`, …) so the
screens and `WebShellTests` find them by the same names.

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
