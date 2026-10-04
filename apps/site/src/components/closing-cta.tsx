import { DownloadButton } from "@/components/download-button";
import { platformIcon } from "@/components/platform-icons";
import { platformIds, platforms, site } from "@/lib/site";

const rows = platformIds.map((id) => ({
	Icon: platformIcon[id],
	...platforms[id],
}));

export function ClosingCta() {
	return (
		<section
			className="relative overflow-hidden border-border border-t py-24 text-center sm:py-[120px]"
			id="download"
		>
			<div
				aria-hidden="true"
				className="pointer-events-none absolute inset-x-0 -bottom-1/2 h-[60%] opacity-60 [background:radial-gradient(ellipse_at_center,var(--live-dim)_0%,transparent_60%)]"
			/>
			<div className="relative mx-auto max-w-[1240px] px-5 sm:px-8">
				<h2 className="display mx-auto max-w-[16ch] text-balance text-[clamp(40px,6vw,72px)]">
					Your meetings deserve better than a bot.
				</h2>
				<p className="mx-auto mt-5 mb-9 max-w-[560px] text-[18px] text-fg-muted leading-[1.55] tracking-[-0.005em]">
					Steno is free, open source and runs on your machine. Install it, point
					it at a model, and the next call writes its own notes.
				</p>
				<div className="mb-6 flex flex-wrap justify-center gap-2.5">
					<DownloadButton />
					<a
						className="btn btn-ghost btn-lg"
						href={site.releases}
						rel="noreferrer"
						target="_blank"
					>
						All downloads
					</a>
				</div>
				<ul className="mx-auto mt-10 grid max-w-[720px] gap-px overflow-hidden rounded-lg border border-border bg-border sm:grid-cols-3">
					{rows.map(({ Icon, name, note }) => (
						<li
							className="flex items-center gap-3 bg-bg px-5 py-4 text-left"
							key={name}
						>
							<Icon className="size-4 shrink-0 text-fg-muted" />
							<span className="min-w-0">
								<span className="block font-medium text-[14px] tracking-[-0.01em]">
									{name}
								</span>
								<span className="block font-mono text-[11px] text-fg-dim">
									{note}
								</span>
							</span>
						</li>
					))}
				</ul>
			</div>
		</section>
	);
}
