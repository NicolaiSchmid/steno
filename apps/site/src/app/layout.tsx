import type { Metadata, Viewport } from "next";
import { DM_Sans, JetBrains_Mono } from "next/font/google";
import type { ReactNode } from "react";
import { site } from "@/lib/site";
import "./globals.css";

/* DM Sans and JetBrains Mono, as on t3.codes; next/font self-hosts them. */
const sans = DM_Sans({
	subsets: ["latin", "latin-ext"],
	weight: ["400", "500", "600", "700"],
	variable: "--font-dm-sans",
	display: "swap",
});

const mono = JetBrains_Mono({
	subsets: ["latin", "latin-ext"],
	weight: ["400", "500"],
	variable: "--font-jetbrains-mono",
	display: "swap",
});

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
		type: "website",
		url: site.url,
		siteName: site.name,
		title: site.title,
		description: site.description,
		locale: "en_US",
	},
	twitter: {
		card: "summary_large_image",
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
		<html className={`${sans.variable} ${mono.variable}`} lang="en">
			<body className="flex min-h-dvh flex-col overflow-x-clip">
				{children}
			</body>
		</html>
	);
}
