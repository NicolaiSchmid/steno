import { ArrowRight } from "lucide-react";
import { Checklist } from "@/components/checklist";
import { SectionHead } from "@/components/section-head";

const rows: Array<[string, string, "local" | "yours"]> = [
	[
		"Meeting audio",
		"A folder you choose. Deleted per your retention setting.",
		"local",
	],
	[
		"Transcript text",
		"The model endpoint you configured, and only there.",
		"yours",
	],
	["Voice embeddings", "A local SQLite database. Never uploaded.", "local"],
	[
		"Summary and tasks",
		"Markdown, VTT and JSON on your disk. Steno never syncs.",
		"local",
	],
	[
		"Phone recordings",
		"Your phone to your computer over the local network. TLS, pinned keys.",
		"local",
	],
];

export function Privacy() {
	return (
		<section className="border-border border-t py-16 sm:py-24" id="privacy">
			<div className="mx-auto grid max-w-[1240px] items-center gap-10 px-5 sm:px-8 lg:grid-cols-[1fr_1.1fr] lg:gap-16">
				<div>
					<SectionHead
						className="mb-7"
						eyebrow="Privacy"
						title="There is no Steno server."
					>
						Every row on the right is a design constraint, not a policy page.
						Any code path that sends bytes over the network is a destination you
						configured, or the text-only model client. Audio has no way out.
					</SectionHead>
					<Checklist
						items={[
							"Works offline with a local model",
							"Audio retention is a setting, with a per-meeting override",
							"Plain files you can grep, sync and back up yourself",
						]}
					/>
					<a
						className="mt-7 inline-flex items-center gap-1.5 font-medium text-[14px] text-fg-muted transition-colors duration-[180ms] hover:text-fg"
						href="/recording-law"
					>
						Whether you may record a call is a separate question
						<ArrowRight className="size-3.5" strokeWidth={1.5} />
					</a>
				</div>
				<div className="tile p-0">
					<div className="flex items-center justify-between border-border border-b px-4 py-2.5 font-mono text-[11px] text-fg-dim">
						<span>What</span>
						<span>Where it goes</span>
					</div>
					<dl>
						{rows.map(([k, v, kind]) => (
							<div
								className="grid grid-cols-[132px_1fr] gap-4 border-border border-t px-4 py-3.5 first:border-t-0 sm:grid-cols-[160px_1fr]"
								key={k}
							>
								<dt className="flex items-start gap-2 font-medium text-[13px] text-fg">
									<span
										className={`mt-1.5 size-1.5 shrink-0 rounded-full ${kind === "local" ? "bg-ok" : "bg-warn"}`}
									/>
									{k}
								</dt>
								<dd className="text-[13px] text-fg-muted leading-[1.5]">{v}</dd>
							</div>
						))}
					</dl>
					<div className="flex flex-wrap gap-5 border-border border-t px-4 py-3 font-mono text-[11px] text-fg-dim">
						<span className="inline-flex items-center gap-2">
							<span className="size-1.5 rounded-full bg-ok" />
							stays on your machine
						</span>
						<span className="inline-flex items-center gap-2">
							<span className="size-1.5 rounded-full bg-warn" />
							goes where you said
						</span>
					</div>
				</div>
			</div>
		</section>
	);
}
