import { Command } from "@/components/command";
import { DownloadButton } from "@/components/download-button";
import { platformIcon } from "@/components/platform-icons";
import { cn } from "@/lib/cn";
import { listedLinuxTargets, platformIds, platforms, site } from "@/lib/site";

const linuxInstalls = listedLinuxTargets.length > 0;

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
				{/* With install lines, Linux takes a full row under macOS and Windows. */}
				<ul
					className={cn(
						"mx-auto mt-10 grid max-w-[720px] grid-cols-1 gap-px overflow-hidden rounded-lg border border-border bg-border",
						linuxInstalls ? "sm:grid-cols-2" : "sm:grid-cols-3",
					)}
				>
					{platformIds.map((id) => {
						const Icon = platformIcon[id];
						const { name, note } = platforms[id];
						const installs = id === "linux" ? listedLinuxTargets : [];
						return (
							<li
								className={cn(
									"flex flex-col justify-center gap-4 bg-bg px-5 py-4 text-left",
									installs.length > 0 && "sm:col-span-2",
								)}
								key={id}
							>
								<div className="flex items-center gap-3">
									<Icon className="size-4 shrink-0 text-fg-muted" />
									<span className="min-w-0">
										<span className="block font-medium text-[14px] tracking-[-0.01em]">
											{name}
										</span>
										<span className="block font-mono text-[11px] text-fg-muted">
											{note}
										</span>
									</span>
								</div>
								{installs.length > 0 && (
									<ul className="flex flex-col gap-2">
										{installs.map(({ name: target, install }) => (
											<li
												className="grid grid-cols-1 items-center gap-x-4 gap-y-1 sm:grid-cols-[72px_minmax(0,1fr)]"
												key={target}
											>
												<span className="font-mono text-[11px] text-fg-dim">
													{target}
												</span>
												<Command
													className="w-full"
													command={install.line}
													shell={install.shell}
												/>
											</li>
										))}
									</ul>
								)}
							</li>
						);
					})}
				</ul>
			</div>
		</section>
	);
}
