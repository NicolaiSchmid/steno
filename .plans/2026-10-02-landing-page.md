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
   `next build`. `apps/site/vercel.json` carries the same install and build
   commands (upstream keeps them in `vercel.ts`), so on Vercel the project is
   the repository with Root Directory `apps/site`, and the CNAME
   `steno.nicolaischmid.com` points at it. Elsewhere: upload `out/`.

## Out of scope

Blog, docs pages, analytics, a download counter, a newsletter. Each would be
a new plan.
