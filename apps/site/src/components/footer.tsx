import { RecordMark } from "@/components/record-mark";
import { site } from "@/lib/site";

const links: Array<{ href: string; label: string; internal?: boolean }> = [
	{ href: site.repo, label: "GitHub" },
	{ href: site.releases, label: "Download" },
	{ href: site.issues, label: "Issues" },
	{ href: site.scope, label: "Scope" },
	{ href: "/recording-law", label: "Recording law", internal: true },
	{ href: site.tap, label: "Homebrew tap" },
	{ href: site.author.url, label: site.author.name },
];

export function Footer() {
	return (
		<footer className="mt-auto border-border border-t pt-10 pb-8">
			<div className="mx-auto flex max-w-[1240px] flex-col items-start gap-7 px-5 sm:px-8 lg:flex-row lg:flex-wrap lg:items-center lg:justify-between">
				<div className="flex items-center gap-2.5 text-[13px] text-fg-dim">
					<RecordMark className="text-fg" />
					<span>
						© {new Date().getFullYear()} Nicolai Schmid · MIT licensed
					</span>
				</div>
				<nav
					aria-label="Footer"
					className="grid w-full grid-cols-3 gap-x-5 gap-y-3.5 lg:flex lg:w-auto lg:flex-wrap lg:justify-end"
				>
					{links.map((l) => (
						<a
							className="text-[13px] text-fg-dim transition-colors duration-200 hover:text-fg"
							href={l.href}
							key={l.href}
							rel="noreferrer"
							target="_blank"
						>
							{l.label}
						</a>
					))}
				</nav>
			</div>
		</footer>
	);
}
