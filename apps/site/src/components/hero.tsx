import { ArrowUpRight } from "lucide-react";
import { Command } from "@/components/command";
import { DownloadButton } from "@/components/download-button";
import { GitHubMark } from "@/components/github-mark";
import { ProductFrame } from "@/components/product-frame";
import { site } from "@/lib/site";

export function Hero() {
	return (
		<section
			className="relative overflow-hidden pt-14 pb-16 sm:pt-[72px]"
			id="top"
		>
			<div aria-hidden="true" className="absolute inset-0 bg-hero-grid" />
			<div className="relative mx-auto max-w-[1240px] px-5 text-center sm:px-8">
				<p className="eyebrow" data-rise>
					macOS · Windows · Linux · Open source
				</p>
				<h1 className="display mx-auto mt-6 mb-[22px] max-w-[20ch] text-balance text-[clamp(40px,11vw,56px)] sm:text-[clamp(38px,5.6vw,76px)]">
					{/* Inline boxes ignore transforms, so each rising line is a block. */}
					<span className="inline-block" data-rise>
						Meeting notes
					</span>
					<br />
					<span
						className="inline-block"
						data-rise
						style={{ "--d": "90ms" } as React.CSSProperties}
					>
						without the bot.
					</span>
				</h1>
				<p
					className="mx-auto mb-9 max-w-[640px] text-[clamp(16px,1.4vw,19px)] text-fg-muted leading-[1.55] tracking-[-0.005em]"
					data-rise
					style={{ "--d": "300ms" } as React.CSSProperties}
				>
					Steno records the call from your computer&apos;s own audio,
					transcribes and summarises it on-device, and writes plain Markdown you
					own. Nobody sees a notetaker join. Bring your own model. Fork the
					whole thing.
				</p>
				<div
					className="mb-14 flex flex-col items-center gap-4"
					data-rise
					style={{ "--d": "400ms" } as React.CSSProperties}
				>
					<div className="flex flex-wrap items-center justify-center gap-2.5">
						<DownloadButton />
						<a className="btn btn-ghost btn-lg" href="#download">
							All platforms
						</a>
					</div>
					<a
						className="group inline-flex items-center gap-2 font-medium text-[14px] text-fg-muted tracking-[-0.01em] transition-colors duration-[180ms] hover:text-fg"
						href={site.repo}
						rel="noreferrer"
						target="_blank"
					>
						<GitHubMark className="size-3.5 shrink-0" />
						Steal our code (MIT says you may)
						<ArrowUpRight
							className="size-3 shrink-0 opacity-60 transition-[transform,opacity] duration-[180ms] group-hover:translate-x-0.5 group-hover:-translate-y-0.5 group-hover:opacity-100"
							strokeWidth={2}
						/>
					</a>
					<div className="hidden flex-wrap items-center justify-center gap-x-2 gap-y-2 text-[13px] text-fg-dim tracking-[-0.005em] sm:inline-flex">
						<span>Also via Homebrew:</span>
						<Command command={site.brew} />
					</div>
				</div>
			</div>
			{/* The rise keyframe ends on `transform: none`, so the tilt lives one level in. */}
			<div
				className="relative mx-auto hidden max-w-[1180px] px-5 sm:block sm:px-8"
				data-rise
				style={{ "--d": "520ms" } as React.CSSProperties}
			>
				<div className="hero-preview">
					<ProductFrame />
				</div>
			</div>
		</section>
	);
}
