import type { Metadata, Viewport } from "next";
import type { ReactNode } from "react";
import { openGraphBase, twitterBase } from "@/lib/share";
import { site } from "@/lib/site";
import "./globals.css";

export const metadata: Metadata = {
	metadataBase: new URL(site.url),
	title: {
		default: site.title,
		template: `%s · ${site.name}`,
	},
	description: site.description,
	applicationName: site.name,
	authors: [{ name: site.author.name, url: site.author.url }],
	keywords: [
		"meeting recorder",
		"bot-free",
		"on-device transcription",
		"macOS",
		"Windows",
		"Linux",
		"open source",
		"Parakeet",
		"Whisper",
	],
	alternates: { canonical: "/" },
	openGraph: {
		...openGraphBase,
		url: site.url,
		title: site.title,
		description: site.description,
	},
	twitter: {
		...twitterBase,
		title: site.title,
		description: site.description,
	},
	robots: { index: true, follow: true },
};

export const viewport: Viewport = {
	themeColor: "#09090b",
	colorScheme: "dark",
	width: "device-width",
	initialScale: 1,
};

export default function RootLayout({ children }: { children: ReactNode }) {
	return (
		<html lang="en">
			<head>
				{/* The latin DM Sans file carries the headline; everything else may swap in. */}
				<link
					as="font"
					crossOrigin="anonymous"
					href="/fonts/dm-sans-latin.woff2"
					rel="preload"
					type="font/woff2"
				/>
			</head>
			<body className="flex min-h-dvh flex-col overflow-x-clip">
				{children}
			</body>
		</html>
	);
}
