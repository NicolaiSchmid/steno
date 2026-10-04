import { ArrowUpRight } from "lucide-react";
import { SectionHead } from "@/components/section-head";
import { cases } from "@/lib/recording-law";

/** The 2025–2026 cases against AI notetakers and what each means here. */
export function Cases() {
	return (
		<section className="border-border border-t py-16 sm:py-24" id="cases">
			<div className="mx-auto max-w-[1240px] px-5 sm:px-8">
				<SectionHead eyebrow="In court" title="What the notetaker cases say.">
					Every case so far targets a cloud service. Each one still shows which
					design choices draw claims, and which of them apply to a recorder that
					runs on your own computer.
				</SectionHead>
				<div className="tile p-0">
					{cases.map((c) => (
						<div
							className="grid gap-4 border-border border-t p-5 first:border-t-0 lg:grid-cols-[minmax(0,1fr)_minmax(0,1.4fr)_minmax(0,1.2fr)] lg:gap-8"
							key={c.name}
						>
							<div>
								<a
									className="inline-flex items-center gap-1.5 font-medium text-[14px] text-fg tracking-[-0.01em] transition-colors duration-[180ms] hover:text-white"
									href={c.sources[0].href}
									rel="noreferrer"
									target="_blank"
								>
									{c.name}
									<ArrowUpRight
										className="size-3 text-fg-dim"
										strokeWidth={1.5}
									/>
								</a>
								<div className="mt-1 font-mono text-[11px] text-fg-dim">
									{c.when}
								</div>
							</div>
							<p className="text-[13px] text-fg-muted leading-[1.6]">
								{c.held}
							</p>
							<p className="border-border border-l pl-4 text-[13px] text-fg leading-[1.6]">
								<span className="mb-1 block font-mono text-[10px] text-fg-dim uppercase tracking-[0.08em]">
									For Steno
								</span>
								{c.forSteno}
							</p>
						</div>
					))}
				</div>
			</div>
		</section>
	);
}
