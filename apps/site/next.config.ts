import type { NextConfig } from "next";

/**
 * The landing page is a static site: every route is rendered at build time
 * into `out/`, so it can be served from any static host (Vercel, Cloudflare
 * Pages, a plain web server) at steno.nicolaischmid.com.
 *
 * `NEXT_DEV_ALLOWED_ORIGINS` lists the hosts a developer opens the dev server
 * from besides localhost (a tailnet or LAN name); without them Next blocks its
 * own chunks and nothing hydrates.
 */
const allowedDevOrigins = (process.env.NEXT_DEV_ALLOWED_ORIGINS ?? "")
	.split(",")
	.map((origin) => origin.trim())
	.filter(Boolean);

const nextConfig: NextConfig = {
	output: "export",
	...(allowedDevOrigins.length > 0 ? { allowedDevOrigins } : {}),
	trailingSlash: false,
	images: { unoptimized: true },
	reactStrictMode: true,
	// The repository root AGENTS.md is the source of agent guidance.
	agentRules: false,
};

export default nextConfig;
