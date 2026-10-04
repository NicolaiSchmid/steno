import type { MetadataRoute } from "next";
import { site } from "@/lib/site";

export const dynamic = "force-static";

export default function sitemap(): MetadataRoute.Sitemap {
	return [
		{ url: site.url, changeFrequency: "weekly", priority: 1 },
		{
			url: `${site.url}${site.recordingLaw}`,
			changeFrequency: "monthly",
			priority: 0.6,
		},
	];
}
