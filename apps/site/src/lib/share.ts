import { site } from "@/lib/site";

/*
 * The share-card fields every page repeats. A page that sets `openGraph` or
 * `twitter` replaces the layout's object whole instead of merging it, and
 * loses the root opengraph-image with it, so pages spread these back in.
 */
export const openGraphBase = {
	type: "website",
	siteName: site.name,
	locale: "en_US",
} as const;

export const twitterBase = { card: "summary_large_image" } as const;
