import { reviewed, sources } from "@/lib/recording-law";
import { site } from "@/lib/site";

/** Where the claims on this page come from. */
export function Sources() {
	return (
		<section className="border-border border-t py-16 sm:py-20" id="sources">
			<div className="mx-auto max-w-[1240px] px-5 sm:px-8">
				<h2 className="eyebrow mb-5">Sources · reviewed {reviewed}</h2>
				<ul className="grid gap-x-8 gap-y-2.5 md:grid-cols-2">
					{sources.map((s) => (
						<li key={s.href}>
							<a
								className="text-[13px] text-fg-muted underline decoration-border-strong underline-offset-4 transition-colors duration-[180ms] hover:text-fg"
								href={s.href}
								rel="noreferrer"
								target="_blank"
							>
								{s.label}
							</a>
						</li>
					))}
				</ul>
				<p className="mt-8 max-w-[720px] text-[13px] text-fg-muted leading-[1.6]">
					The case summaries and the German rows rest on these sources. The
					other countries are general summaries of their statutes, not checked
					against recent case law. Found something wrong?{" "}
					<a
						className="underline decoration-border-strong underline-offset-4 transition-colors duration-[180ms] hover:text-fg"
						href={site.issues}
						rel="noreferrer"
						target="_blank"
					>
						Open an issue
					</a>{" "}
					and it gets fixed here.
				</p>
			</div>
		</section>
	);
}
