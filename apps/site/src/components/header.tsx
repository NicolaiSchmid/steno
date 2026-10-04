"use client";

import { useEffect, useState } from "react";
import { GitHubMark } from "@/components/github-mark";
import { RecordMark } from "@/components/record-mark";
import { cn } from "@/lib/cn";
import { site } from "@/lib/site";

const nav = [
	{ href: "#how", label: "How it works" },
	{ href: "#privacy", label: "Privacy" },
	{ href: "#built", label: "Platforms" },
	{ href: "#open", label: "Source" },
];

/** The sticky nav: wordmark, links from 768 px, Download, the GitHub pill. */
export function Header() {
	const [scrolled, setScrolled] = useState(false);

	useEffect(() => {
		const onScroll = () => setScrolled(window.scrollY > 12);
		window.addEventListener("scroll", onScroll, { passive: true });
		onScroll();
		return () => window.removeEventListener("scroll", onScroll);
	}, []);

	return (
		<header
			className={cn(
				"nav-glass sticky top-0 z-50 border-b transition-colors duration-300",
				scrolled ? "border-border" : "border-transparent",
			)}
		>
			<div className="mx-auto flex max-w-[1240px] items-center justify-between gap-6 px-5 py-3 sm:px-8">
				<a
					aria-label="Steno home"
					className="inline-flex items-center gap-2.5 font-[650] text-[16px] text-fg leading-none tracking-[-0.025em] transition-colors duration-200 hover:text-white"
					href="#top"
				>
					<RecordMark />
					Steno
				</a>
				<nav aria-label="Primary" className="hidden items-center md:flex">
					{nav.map((item) => (
						<a
							className="whitespace-nowrap px-2.5 text-[13px] text-fg-muted tracking-[-0.01em] transition-colors duration-[180ms] hover:text-fg"
							href={item.href}
							key={item.href}
						>
							{item.label}
						</a>
					))}
				</nav>
				<div className="inline-flex items-center gap-2">
					<a
						className="px-2.5 text-[13px] text-fg-muted tracking-[-0.01em] transition-colors duration-[180ms] hover:text-fg"
						href="#download"
					>
						Download
					</a>
					<a
						aria-label="Steno on GitHub"
						className="inline-flex h-9 items-center gap-2 whitespace-nowrap rounded-full border border-border bg-white/[0.02] px-3.5 text-[13px] text-fg-muted tracking-[-0.01em] transition-colors duration-[180ms] hover:border-border-strong hover:bg-white/[0.04] hover:text-fg"
						href={site.repo}
						rel="noreferrer"
						target="_blank"
					>
						<GitHubMark className="size-3.5" />
						<span>
							<strong className="font-semibold text-fg">GitHub</strong>
							<span className="hidden sm:inline"> · MIT</span>
						</span>
					</a>
				</div>
			</div>
		</header>
	);
}
