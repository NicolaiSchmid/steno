import { Checklist } from "@/components/checklist";
import { SectionHead } from "@/components/section-head";

const providers = [
	{ name: "Ollama", tag: "localhost:11434", local: true },
	{ name: "LM Studio", tag: "localhost:1234", local: true },
	{ name: "ChatGPT", tag: "your own plan" },
	{ name: "OpenAI API", tag: "api.openai.com" },
	{ name: "OpenRouter", tag: "openrouter.ai" },
	{ name: "Anything else", tag: "compatible URL" },
];

export function ByoModel() {
	return (
		<section className="border-border border-t py-16 sm:py-24" id="model">
			<div className="mx-auto max-w-[1240px] px-5 sm:px-8">
				<SectionHead title="Bring your own model">
					Steno doesn&apos;t resell tokens. Transcription and speaker
					recognition run on your machine; the summary, the task list and the
					Denglish cleanup go to whatever model you point it at. A base URL and
					a key, nothing else.
				</SectionHead>
				<div className="grid grid-cols-2 overflow-hidden rounded-lg border border-border lg:grid-cols-6">
					{providers.map((p, i) => (
						<div
							className={`flex items-center gap-3.5 border-border bg-gradient-to-b from-white/[0.015] to-transparent px-5 py-[22px] transition-colors duration-200 hover:bg-white/[0.025] ${
								i % 2 === 0 ? "border-r lg:border-r" : ""
							} ${i < providers.length - 2 ? "border-b lg:border-b-0" : ""} ${
								i < providers.length - 1 ? "lg:border-r" : ""
							}`}
							key={p.name}
						>
							<span className="grid size-7 shrink-0 place-items-center rounded-sm border border-border bg-white/[0.02] font-mono text-[11px] text-fg-muted">
								{p.local ? "⌂" : "⇄"}
							</span>
							<div className="min-w-0 flex-1">
								<div className="mb-0.5 truncate font-medium text-[14px] tracking-[-0.01em]">
									{p.name}
								</div>
								<div className="truncate font-mono text-[11px] text-fg-dim">
									{p.tag}
								</div>
							</div>
						</div>
					))}
				</div>
				<div className="mt-7 border-border border-t border-dashed pt-6">
					<Checklist
						items={[
							"No account. No subscription. No quota caps.",
							"Fully offline with a local model.",
							"Only transcript text ever leaves the machine, and only to the endpoint you chose.",
						]}
					/>
				</div>
			</div>
		</section>
	);
}
