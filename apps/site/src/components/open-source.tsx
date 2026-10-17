import { ArrowUpRight, Check, GitFork } from "lucide-react";
import { SectionHead } from "@/components/section-head";
import { site } from "@/lib/site";

const pitch = [
	[
		"Change the templates.",
		"Summaries follow Markdown templates you can edit or add.",
	],
	["Add a destination.", "Write meetings to whatever your notes live in."],
	["Ship your own build.", "Sign it yourself or install it from source."],
];

export function OpenSource() {
	return (
		<section className="border-border border-t py-16 sm:py-24" id="open">
			<div className="mx-auto max-w-[1240px] px-5 sm:px-8">
				<SectionHead
					align="center"
					eyebrow="Open source"
					title="If you don't like something, fork it."
				/>
				<div className="grid gap-5 lg:grid-cols-[1.25fr_1fr]">
					<div className="tile p-0">
						<div className="flex items-center gap-1.5 border-border border-b bg-white/[0.02] px-3.5 py-2.5">
							<span className="size-2.5 rounded-full bg-[#2a2a30]" />
							<span className="size-2.5 rounded-full bg-[#2a2a30]" />
							<span className="size-2.5 rounded-full bg-[#2a2a30]" />
							<span className="ml-2.5 flex-1 text-center font-mono text-[11px] text-fg-dim">
								~/code
							</span>
						</div>
						<div className="overflow-hidden px-5 py-5 font-mono text-[13px] leading-[1.7] [&>div]:truncate">
							<div>
								<span className="mr-2 text-live-text">$</span>gh repo fork
								NicolaiSchmid/steno --clone
							</div>
							<div className="text-fg-dim">✓ Cloned steno into ./steno</div>
							<div>
								<span className="mr-2 text-live-text">$</span>cd steno
								&amp;&amp; cat AGENTS.md
							</div>
							<div className="text-fg-dim">
								# Steno · bot-free meeting recorder. Scope and every settled
								decision live in .plans/
							</div>
							<div>
								<span className="mr-2 text-live-text">$</span>cargo test
								--workspace
							</div>
							<div className="text-fg-dim">
								✓ steno-core · steno-audio · steno-speech · steno-llm ·{" "}
								<span className="text-live-text">all green</span>
							</div>
							<div className="flex items-center">
								<span className="mr-2 text-live-text">$</span>
								<span className="ml-1 inline-block h-3.5 w-[7px] animate-blink bg-fg" />
							</div>
						</div>
					</div>
					<div className="tile flex flex-col justify-center gap-8 bg-[radial-gradient(110%_75%_at_100%_0%,var(--live-dim),transparent_60%),linear-gradient(180deg,rgba(255,255,255,0.02),transparent)] p-8">
						<ul className="flex flex-col gap-4">
							{pitch.map(([strong, rest]) => (
								<li
									className="flex items-start gap-3 text-[15px] text-fg-muted leading-[1.5]"
									key={strong}
								>
									<span className="mt-px grid size-[22px] shrink-0 place-items-center rounded-[7px] border border-live/30 bg-live-dim text-live-text">
										<Check className="size-3" strokeWidth={2.4} />
									</span>
									<span>
										<strong className="font-semibold text-fg">{strong}</strong>{" "}
										{rest}
									</span>
								</li>
							))}
						</ul>
						<div className="flex flex-col gap-[18px]">
							<div className="flex items-center gap-2 font-mono text-[11px] text-fg-dim">
								<span>MIT licensed</span>
								<span aria-hidden="true">·</span>
								<span>Rust core · Tauri shell</span>
							</div>
							<div className="flex flex-wrap items-center gap-4">
								<a
									className="btn btn-primary"
									href={site.fork}
									rel="noreferrer"
									target="_blank"
								>
									<GitFork className="size-3.5" strokeWidth={1.7} />
									Fork on GitHub
								</a>
								<a
									className="inline-flex items-center gap-1.5 font-medium text-[13px] text-fg-muted transition-colors duration-[180ms] hover:text-fg"
									href={site.repo}
									rel="noreferrer"
									target="_blank"
								>
									Browse source
									<ArrowUpRight className="size-3" strokeWidth={1.5} />
								</a>
							</div>
						</div>
					</div>
				</div>
			</div>
		</section>
	);
}
