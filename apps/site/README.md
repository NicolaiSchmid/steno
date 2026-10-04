# Steno landing page

The marketing site for [steno.nicolaischmid.com](https://steno.nicolaischmid.com):
the home page and `/recording-law`, built with Next.js 16 (App Router, static export), React 19,
Tailwind CSS 4 and Biome. It is the first member of the root pnpm workspace,
laid out like pingdotgg/t3code's `apps/marketing`.

## Commands

From the repository root:

```sh
pnpm install          # once per checkout; root lockfile
pnpm dev:site         # http://localhost:3000
pnpm check:site       # biome lint + typecheck (what CI runs)
pnpm build:site       # static site in apps/site/out/
```

Inside `apps/site/` the same scripts are `pnpm dev`, `pnpm check` and
`pnpm build`.

`pnpm build` writes a fully static site to `out/`, including the Open Graph
image, `robots.txt` and `sitemap.xml`. No server is needed to host it.

Opening the dev server from another host (a tailnet or LAN name) needs that
host in `NEXT_DEV_ALLOWED_ORIGINS`, comma-separated, or nothing hydrates.

## Hosting

The Vercel project (team wasc, `steno`) has the repository root as its Root
Directory, and the root `vercel.json` carries the install command (filtered
to `@steno/site...`), `pnpm build:site`, the output directory `apps/site/out`
and an ignore command that skips every commit without `apps/site` or without
changes to it. The root directory has to be the repository root: Vercel
rejects a commit whose configured root directory does not exist before it
runs any ignore step, and the Rust branches have no `apps/site`. On
Cloudflare Pages or a plain web server, upload `out/`.

URLs, install commands and the platform labels live in `src/lib/site.ts`.
Change them there, nowhere else.

The facts on `/recording-law` (the country rows, cases, sources and the
`reviewed` date) live in `src/lib/recording-law.ts`. Change them there, and
update `reviewed` whenever you re-check a row. When the app closes one of the
gaps that page lists, delete its row.

## Design

The page follows t3.codes: dark only on `#09090b`, DM Sans and JetBrains Mono
(self-hosted from `public/fonts`, OFL), a five-step neutral text ladder, hairline
borders at 8 and 14 % white, white primary buttons, a `[data-rise]` entrance.
Tokens and the few shared classes (`.btn`, `.tile`, `.eyebrow`, `.display`)
are in `src/app/globals.css`. Red, the app's live colour, is the only hue.

The product mock in the hero (`src/components/product-frame.tsx`) uses the
example meeting from the repository README. Nothing in it comes from a real
recording.
