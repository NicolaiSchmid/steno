import { ArrowUpRight } from "lucide-react";
import { SectionHead } from "@/components/section-head";
import { gaps } from "@/lib/recording-law";
import { site } from "@/lib/site";

/** What the app leaves to you today, said plainly. */
export function Gaps() {
	return (
		<section className="border-border border-t py-16 sm:py-24" id="gaps">
			<div className="mx-auto grid max-w-[1240px] items-start gap-10 px-5 sm:px-8 lg:grid-cols-[1fr_1.1fr] lg:gap-16">
				<SectionHead
					className="mb-0"
					eyebrow="Your part"
					title="What Steno doesn't do for you."
				>
					Steno is a tool you run, not a service that takes on your duties.
					These are the parts it leaves to you today.
				</SectionHead>
				<div className="tile p-0">
					<dl>
						{gaps.map((g) => (
							<div
								className="border-border border-t px-5 py-4 first:border-t-0"
								key={g.title}
							>
								<dt className="mb-1 font-medium text-[15px] tracking-[-0.01em]">
									{g.title}
								</dt>
								<dd className="text-[14px] text-fg-muted leading-[1.6]">
									{g.body}
								</dd>
							</div>
						))}
					</dl>
					<a
						className="flex items-center gap-1.5 border-border border-t px-5 py-3 font-mono text-[11px] text-fg-dim transition-colors duration-[180ms] hover:text-fg"
						href={site.issues}
						rel="noreferrer"
						target="_blank"
					>
						Ask for one of these on GitHub
						<ArrowUpRight className="size-3" strokeWidth={1.5} />
					</a>
				</div>
			</div>
		</section>
	);
}
