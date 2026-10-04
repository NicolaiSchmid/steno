import { SectionHead } from "@/components/section-head";
import { cn } from "@/lib/cn";
import { type Consent, consentLabel, jurisdictions } from "@/lib/recording-law";

const dot: Record<Consent, string> = {
	one: "bg-ok",
	mixed: "bg-warn",
	all: "bg-fg",
};

/** Who has to agree before a participant records, by jurisdiction. */
export function ConsentTable() {
	return (
		<section className="border-border border-t py-16 sm:py-24" id="countries">
			<div className="mx-auto max-w-[1240px] px-5 sm:px-8">
				<SectionHead eyebrow="By country" title="Who has to agree?">
					The question each row answers: may one person on the call record it
					without telling the others? Where people sit in different places,
					follow the strictest rule among them.
				</SectionHead>
				<div className="tile p-0">
					<div className="hidden grid-cols-[minmax(0,1.1fr)_170px_minmax(0,2fr)] gap-6 border-border border-b px-5 py-2.5 font-mono text-[11px] text-fg-dim md:grid">
						<span>Where</span>
						<span>A participant…</span>
						<span>What to know</span>
					</div>
					<dl>
						{jurisdictions.map((j) => (
							<div
								className="grid gap-2 border-border border-t px-5 py-4 first:border-t-0 md:grid-cols-[minmax(0,1.1fr)_170px_minmax(0,2fr)] md:gap-6"
								key={j.place}
							>
								<dt className="font-medium text-[14px] text-fg leading-[1.45] tracking-[-0.01em]">
									{j.place}
								</dt>
								<dd className="inline-flex items-start gap-2 text-[13px] text-fg-muted">
									<span
										className={cn(
											"mt-1.5 size-1.5 shrink-0 rounded-full",
											dot[j.consent],
										)}
									/>
									{consentLabel[j.consent]}
								</dd>
								<dd className="text-[13px] text-fg-muted leading-[1.55]">
									{j.note}
								</dd>
							</div>
						))}
					</dl>
					<div className="flex flex-wrap gap-5 border-border border-t px-5 py-3 font-mono text-[11px] text-fg-dim">
						{(["one", "mixed", "all"] as const).map((kind) => (
							<span className="inline-flex items-center gap-2" key={kind}>
								<span className={cn("size-1.5 rounded-full", dot[kind])} />
								{consentLabel[kind].toLowerCase()}
							</span>
						))}
					</div>
				</div>
				<p className="mt-5 max-w-[720px] text-[14px] text-fg-dim leading-[1.6]">
					&ldquo;You may record&rdquo; covers capturing a call you take part in.
					Publishing or sharing the recording is a separate question almost
					everywhere, and work use brings in data protection law on top.
				</p>
			</div>
		</section>
	);
}
