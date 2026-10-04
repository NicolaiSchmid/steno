import { reviewed, summary } from "@/lib/recording-law";

/** The page head: what this page answers, the short version, the caveat. */
export function LawHero() {
	return (
		<section className="relative overflow-hidden pt-14 pb-16 sm:pt-[72px] sm:pb-20">
			<div aria-hidden="true" className="absolute inset-0 bg-hero-grid" />
			<div className="relative mx-auto max-w-[1240px] px-5 sm:px-8">
				<p className="eyebrow" data-rise>
					Recording and the law
				</p>
				<h1
					className="display mt-6 mb-[22px] max-w-[18ch] text-balance text-[clamp(36px,5vw,64px)]"
					data-rise
				>
					Local notes are private. Recording is still your call.
				</h1>
				<p
					className="mb-12 max-w-[680px] text-[clamp(16px,1.4vw,19px)] text-fg-muted leading-[1.55] tracking-[-0.005em]"
					data-rise
					style={{ "--d": "120ms" } as React.CSSProperties}
				>
					Steno records and transcribes on your computer and never uploads
					audio. That settles who can hear your meetings. It doesn&apos;t settle
					whether you may record the people in them. This page explains what the
					law asks of you, country by country and situation by situation, and
					what to say before you press Record.
				</p>
				<div
					className="grid gap-px overflow-hidden rounded-lg border border-border bg-border lg:grid-cols-3"
					data-rise
					style={{ "--d": "220ms" } as React.CSSProperties}
				>
					{summary.map(({ title, body }) => (
						<div className="bg-bg p-6" key={title}>
							<div className="mb-2 font-medium text-[15px] tracking-[-0.01em]">
								{title}
							</div>
							<p className="text-[14px] text-fg-muted leading-[1.55]">{body}</p>
						</div>
					))}
				</div>
				<p className="mt-6 max-w-[680px] font-mono text-[11px] text-fg-dim leading-[1.7]">
					Not legal advice. Laws change and courts read them differently; check
					with a lawyer before you rely on a row here for work. Last reviewed{" "}
					{reviewed}.
				</p>
			</div>
		</section>
	);
}
