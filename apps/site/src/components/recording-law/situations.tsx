import { SectionHead } from "@/components/section-head";
import { situations } from "@/lib/recording-law";

/** The same rules, read for the four ways people use Steno. */
export function Situations() {
	return (
		<section className="border-border border-t py-16 sm:py-24" id="situations">
			<div className="mx-auto max-w-[1240px] px-5 sm:px-8">
				<SectionHead eyebrow="By situation" title="What changes with context.">
					The more your notes are for work, and the more people you record who
					don&apos;t know you, the more the law asks.
				</SectionHead>
				<div className="grid gap-px overflow-hidden rounded-lg border border-border bg-border md:grid-cols-2 lg:grid-cols-4">
					{situations.map((s) => (
						<div className="bg-bg p-6" key={s.title}>
							<div className="mb-2.5 font-medium text-[15px] tracking-[-0.01em]">
								{s.title}
							</div>
							<p className="text-[14px] text-fg-muted leading-[1.6]">
								{s.body}
							</p>
						</div>
					))}
				</div>
			</div>
		</section>
	);
}
