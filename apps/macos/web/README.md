# Steno web UI

The pixels of the Mac windows: one React app, bundled by Vite and served by
the app from its own URL scheme. Swift keeps the state and the audio; the page
draws snapshots and sends commands over the bridge (`src/bridge/`). Plan:
`.plans/2026-09-29-macos-webview-ui.md`. Spec for the look:
`.plans/2026-10-01-t3code-design-polish.md`.

## Commands

| Command | What it does |
|---|---|
| `pnpm install` | pnpm 11, Node 24. Own lockfile. |
| `pnpm dev` | Vite dev server on 5173 with the fixture bridge. `#/main` (the default), `#/settings?section=general\|recording\|transcription\|summaries\|export\|iphone`, `#/onboarding`, `#/stories`. Flags: `dark`, `platform=macos\|windows\|linux` (the words and keys of that OS; the Mac without it), `tab=summary\|transcript\|tasks\|notes`, `menu`, `picker`, `scenario=…` (below), for example `#/main?dark&tab=transcript&picker`, `#/settings?section=iphone&scenario=pairing` or `#/onboarding?scenario=onboarding-codex`. |
| `pnpm check` | `lint`, `lint:ui`, `typecheck`, `test`. Must pass before a PR. |
| `pnpm build` | Writes `dist/`, the bundle the app ships, with relative asset URLs and without the mock bridge or the fixtures; then `scripts/check-offline.mjs` greps it for fetchable URLs and `scripts/check-bundle.mjs` for any trace of the mock or a fixture. |
| `pnpm build:screens` | Writes `dist-screens/`: the same pages over the fixture bridge (`vite build --mode screens`), for the screens and `vite preview --mode screens`. |
| `pnpm screens` | Builds the screens bundle, then Playwright renders the main window in every state at 960 by 600 and 1200 by 760, the Settings window's sections and states at its default 960 by 640, the onboarding window's two pages and their states at its fixed 560 by 620, and the stories, light and dark, to `screens/` and asserts the page made no network request. Run `pnpm exec playwright install chromium` once. |

## Layout

| Path | What lives there |
|---|---|
| `src/bridge/` | The contract (`contract.ts`), the typed client, the WebKit, Tauri and mock transports, and `hooks.ts` (`useSnapshot`, `useBridge`, `send`). `createBridge()` picks WebKit inside the Swift app, Tauri inside the Tauri shell (`apps/desktop`) and otherwise the `#bridge-fallback` module, which `vite.config.ts` resolves to `fallback-mock.ts` (dev server, Vitest, the screens bundle) or `fallback-none.ts` (the production bundle; it throws). |
| `src/windows/main/` | The main window over the bridge: sidebar, meeting list, detail with its tabs, `format.ts` for every date and duration. |
| `src/lib/platform.tsx` | The platform the page runs on (the Tauri shell's `window.__STENO_PLATFORM__`, else the `platform` flag, else the Swift app's Mac) and every word and key that follows from it: "this Mac" or "this computer", Finder or File Explorer, ⌘F or Ctrl+F. `usePlatform()` gives the words and the shortcut labels, `useShortcut()` binds a key; call sites never branch on the OS. |
| `src/windows/settings/` | The Settings window: a sidebar of the six sections as single-line rows, one `*-section.tsx` per section built from `FormCard` and `FormRow`, `settings-format.ts` for sizes and relative times. Text fields keep a draft (`src/lib/use-draft.ts`) and send one `update` plus a `save` when focus leaves. |
| `src/windows/onboarding/` | The onboarding window over the `onboarding` snapshot: `permissions-page.tsx` (one `PermissionRow` per permission, Later or Done) and `setup-page.tsx` (the Summaries row with the service form or the ChatGPT consent card, the Obsidian vault row with the native chooser, Back and Finish) in the frame `onboarding-page.tsx` draws. The host says which page is current and closes the window itself once `finished` is in the snapshot; the page does not send `window.close`. |
| `src/components/` | Pieces two windows share, built from `ui/`: `permission-row.tsx` (Settings and onboarding), `summaries-endpoint-form.tsx` (the service and its server, model and key fields, laid out as rows or a stack), `codex-consent-card.tsx` (the ChatGPT consent words, once), `draft-field.tsx`. |
| `src/components/ui/` | The component set; the only place a look is defined. |
| `src/stories/` | Every component in every variant, rendered by the screens. |
| `fixtures/bridge/` | Snapshots and replies recorded by the Swift side; the mock transport serves them outside the app. They never enter `dist/`. |

Outside the app, the mock transport answers a `?scenario=` in the page's query by bending the
fixtures (`applyScenario` in `src/bridge/mock-transport.ts`): `empty` clears
the list, `recording` makes `recording.live` the recording, `failed` selects
the failed meeting, `processing` adds and selects a meeting in the progress
entry. `?tab=` picks the detail tab. For Settings: `settings-error` (an error
with its details, a login item awaiting approval, an update available),
`download-failed`, `summaries-connected`, `summaries-failed`, `codex-consent`,
`codex` (from `settings.summaries.codex`), `export-on`, `pairing` (from
`settings.iphone.pairing`) and `phone-unavailable`. For onboarding:
`onboarding-unknown` (a fresh install), `onboarding-denied`,
`onboarding-granted` (Done instead of Later), `onboarding-setup` (page 2 from
`onboarding.setup`: Summaries saved, a vault chosen but refused),
`onboarding-setup-open`, `onboarding-codex` (the consent card) and
`onboarding-vault-saved`. Interactive elements carry the
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
