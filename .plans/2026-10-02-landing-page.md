# Landing page at steno.nicolaischmid.com

Date: 2026-10-02. Status: implemented in `apps/site/`.

## Goal

A public page for Steno that explains the product in one scroll, links the
three install routes and the repository, and renders well as a link preview.
It is hosted at `steno.nicolaischmid.com`.

## Decisions

1. **A root pnpm workspace, laid out like pingdotgg/t3code.** `package.json`,
   `pnpm-workspace.yaml` and `pnpm-lock.yaml` at the repository root;
   `apps/site` is its first member and root scripts (`dev:site`,
   `build:site`, `check:site`) mirror upstream's `dev:marketing` and
   `build:marketing`. `apps/macos/web` and `mobile` stay outside for now, each
   marked standalone by its own `pnpm-workspace.yaml`: the mobile lockfile
   feeds the Expo native fingerprint that decides OTA versus TestFlight, and
   the web UI's lockfile is what `scripts/build-web.sh` installs from inside
   the Xcode build. Moving either in is a plan of its own.
2. **Next.js 16, App Router, `output: "export"`.** Everything is rendered at
   build time into `out/`, including the Open Graph image, `robots.txt` and
   `sitemap.xml`. The site needs no server and can move hosts freely.
3. **Design and structure after t3.codes.** Dark only on `#09090b`, DM Sans
   and JetBrains Mono through `next/font`, the upstream text ladder and
   hairlines, `[data-rise]` entrance, a sticky blurred nav, white primary
   buttons. Page order as upstream: hero with a leaning product frame →
   "How it works" (prompt card and pipeline) → "Bring your own model"
   (provider grid, after "Bring your own sub") → "Privacy" (where data goes)
   → "If you don't like something, fork it." (terminal) → closing CTA →
   one-row footer. Red, Steno's live colour, is the only hue
   (`2026-10-02-neutral-accent.md`).
4. **Content.** Multi-platform: the download button labels itself for the
   visitor's OS (macOS on the server), the closing CTA lists macOS, Windows
   and Linux. Both link to GitHub releases; flip the Windows and Linux rows
   once those builds ship. The vault and Obsidian are not mentioned; output
   is "plain Markdown you own". The hero mock shows the README's example
   meeting, never a real recording.
5. **CI and hosting.** `.github/workflows/site-ci.yml` installs from the root
   lockfile with `--filter @steno/site...` and runs Biome, `tsc` and
   `next build`. The Vercel project (team wasc, `steno`, CNAME
   `steno.nicolaischmid.com`) has the repository root as its Root Directory
   and a root `vercel.json` with the same install command, `pnpm build:site`,
   output `apps/site/out`, framework "Other" (static files) and an ignore
   command. Tried first and rejected: Root Directory `apps/site` with
   `apps/site/vercel.json`. Vercel fails a commit whose root directory does
   not exist before any ignore step runs, so every push to the Rust branches
   (no `apps/site`) produced a failed deployment. With the root at the
   repository root the ignore command sees every branch and skips those
   without the site or without changes to it. The Next.js preset is not used
   because at the repository root Vercel finds no `next` dependency; the
   export is plain static output anyway. Fonts are self-hosted woff2 files in
   `apps/site/public/fonts` (as upstream) because `next/font/google` failed to
   resolve under Turbopack on Vercel's builder.

## Fit with the Rust core and Tauri shell

Checked on 2026-10-02 against `refactor/rust-workspace` and `feat/rust-desktop`
(PRs #151, #153, #155, #156; plan `2026-10-02-rust-core-and-tauri-shell.md`):

- The Cargo workspace (`Cargo.toml`, `crates/`, `apps/desktop/src-tauri`) and
  the pnpm workspace (`package.json`, `pnpm-workspace.yaml`) sit side by side
  at the root and share no files. `apps/*` skips `apps/desktop`, which has no
  `package.json`.
- The Tauri shell builds the web UI with `pnpm dev` and `pnpm build` in
  `apps/macos/web`, and Rust CI installs there with its own lockfile. Both keep
  working under the root workspace because that directory's
  `pnpm-workspace.yaml` marks it standalone (`pnpm -w root` inside it resolves
  to its own `node_modules`). Verified by merging both branches in a scratch
  worktree and running the web install and build.
- WP9 moves the web UI to `apps/web`; it then joins this workspace through
  `apps/*`. That PR drops the standalone workspace file, moves the web
  lockfile into the root one, and changes `tauri.conf.json`'s `frontendDist`
  and build hooks (`pnpm --filter @steno/web build` from the root).
- The only textual overlap is `AGENTS.md`: the Rust PRs rewrite the Core row
  and add a Rust section before "Review guidelines"; this plan's section sits
  before "Mobile" so the two merge cleanly in either order.

## Out of scope

Blog, docs pages, analytics, a download counter, a newsletter. Each would be
a new plan.
