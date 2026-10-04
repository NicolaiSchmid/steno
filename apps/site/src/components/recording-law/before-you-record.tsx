import { CopyButton } from "@/components/copy-button";
import { SectionHead } from "@/components/section-head";
import { disclosures, steps } from "@/lib/recording-law";

/** The practical part, first: what to do and say before recording. */
export function BeforeYouRecord() {
	return (
		<section className="border-border border-t py-16 sm:py-24" id="before">
			<div className="mx-auto grid max-w-[1240px] items-start gap-10 px-5 sm:px-8 lg:grid-cols-[1fr_1.1fr] lg:gap-16">
				<div>
					<SectionHead
						className="mb-8"
						eyebrow="Before you press Record"
						title="Tell people. It is the one step that works everywhere."
					>
						Consent rules differ, but a clear notice at the start is enough in
						most places and the right first step in all of them.
					</SectionHead>
					<ol className="flex flex-col gap-5">
						{steps.map(({ title, body }, i) => (
							<li className="flex gap-4" key={title}>
								<span className="grid size-[22px] shrink-0 place-items-center rounded-[7px] border border-border font-mono text-[11px] text-fg-dim">
									{i + 1}
								</span>
								<div>
									<div className="mb-1 font-medium text-[15px] tracking-[-0.01em]">
										{title}
									</div>
									<p className="text-[14px] text-fg-muted leading-[1.55]">
										{body}
									</p>
								</div>
							</li>
						))}
					</ol>
				</div>
				<div className="tile p-0">
					<div className="border-border border-b px-4 py-2.5 font-mono text-[11px] text-fg-dim">
						Copy and adapt
					</div>
					{disclosures.map((line) => (
						<div
							className="flex items-start gap-3 border-border border-t px-4 py-4"
							key={line.label}
						>
							<div className="min-w-0 flex-1">
								<div className="mb-1.5 font-mono text-[11px] text-fg-dim uppercase tracking-[0.08em]">
									{line.label}
								</div>
								<p className="text-[14px] text-fg leading-[1.6]">{line.text}</p>
							</div>
							<CopyButton label={`Copy "${line.label}"`} text={line.text} />
						</div>
					))}
				</div>
			</div>
		</section>
	);
}
