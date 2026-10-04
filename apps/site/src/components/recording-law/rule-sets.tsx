import { SectionHead } from "@/components/section-head";
import { ruleSets } from "@/lib/recording-law";

/** What Steno keeps, mapped to the body of law that applies to it. */
export function RuleSets() {
	return (
		<section className="border-border border-t py-16 sm:py-24" id="rules">
			<div className="mx-auto max-w-[1240px] px-5 sm:px-8">
				<SectionHead eyebrow="What Steno keeps" title="Three sets of rules.">
					Recording law decides whether you may capture the call. Data
					protection law decides what you may do with the transcript. Biometric
					law covers the voice profiles Steno uses to recognise speakers.
					Keeping everything local helps with the second and third, and does
					nothing for the first.
				</SectionHead>
				<div className="grid gap-5 md:grid-cols-2">
					{ruleSets.map((rule) => (
						<div className="tile p-6" key={rule.what}>
							<div className="mb-4 flex flex-wrap items-center justify-between gap-3">
								<span className="font-medium text-[16px] tracking-[-0.01em]">
									{rule.what}
								</span>
								<span className="rounded-full border border-border px-2.5 py-0.5 font-mono text-[10px] text-fg-dim uppercase tracking-[0.08em]">
									{rule.law}
								</span>
							</div>
							<p className="text-[14px] text-fg-muted leading-[1.6]">
								{rule.detail}
							</p>
						</div>
					))}
				</div>
			</div>
		</section>
	);
}
