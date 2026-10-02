# Steno landing page

The marketing site for [steno.nicolaischmid.com](https://steno.nicolaischmid.com):
one page built with Next.js 16 (App Router, static export), React 19,
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

`vercel.json` holds the install and build commands for a Vercel project whose
Root Directory is `apps/site`; install runs from the workspace root with
`--filter @steno/site...`. Point the `steno.nicolaischmid.com` CNAME at the
project. On Cloudflare Pages or a plain web server, upload `out/`.

URLs, install commands and the platform labels live in `src/lib/site.ts`.
Change them there, nowhere else.

## Design

The page follows t3.codes: dark only on `#09090b`, DM Sans and JetBrains Mono
(self-hosted by `next/font`), a five-step neutral text ladder, hairline
borders at 8 and 14 % white, white primary buttons, a `[data-rise]` entrance.
Tokens and the few shared classes (`.btn`, `.tile`, `.eyebrow`, `.display`)
are in `src/app/globals.css`. Red, the app's live colour, is the only hue.

The product mock in the hero (`src/components/product-frame.tsx`) uses the
example meeting from the repository README. Nothing in it comes from a real
recording.
