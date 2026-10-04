import { Mic } from "lucide-react";
import { Checklist } from "@/components/checklist";
import { RecordMark } from "@/components/record-mark";
import { SectionHead } from "@/components/section-head";

const pipeline = [
	{ step: "Capture", detail: "system audio + mic, two lanes", done: true },
	{ step: "Transcribe", detail: "Parakeet TDT v3 · on-device", done: true },
	{ step: "Diarise", detail: "3 speakers · 2 known voices", done: true },
	{ step: "Summarise", detail: "your model · text only", live: true },
	{ step: "Write", detail: "Markdown · VTT · JSON", pending: true },
];

export function HowItWorks() {
	return (
		<section className="border-border border-t py-16 sm:py-24" id="how">
			<div className="mx-auto grid max-w-[1240px] items-center gap-10 px-5 sm:px-8 lg:grid-cols-[1.1fr_1fr] lg:gap-16">
				<div className="relative flex min-w-0 flex-col items-stretch gap-5">
					{/* The capture prompt */}
					<div className="tile p-5">
						<div className="mb-3 flex items-center gap-2.5">
							<span className="grid size-7 place-items-center rounded-sm bg-live-dim text-live">
								<Mic className="size-3.5" strokeWidth={2} />
							</span>
							<span className="flex-1 font-medium text-[15px]">
								Zoom started using your microphone
							</span>
							<span className="rounded-full border border-border px-2 py-0.5 font-mono text-[10px] text-fg-dim uppercase tracking-[0.08em]">
								14:00
							</span>
						</div>
						<div className="mb-3.5 flex flex-wrap gap-3.5 font-mono text-[11px] text-fg-dim">
							<span>Produktstrategie: „90/10“ &amp; Roadmap für Q4</span>
							<span>3 attendees from your calendar</span>
						</div>
						<div className="flex justify-end gap-2">
							<span className="btn btn-ghost pointer-events-none py-2 text-[13px]">
								Not this one
							</span>
							<span className="btn btn-primary pointer-events-none inline-flex gap-2 py-2 text-[13px]">
								<RecordMark className="h-[11px] [&>i]:w-[2px]" />
								Record
							</span>
						</div>
					</div>

					{/* The trail */}
					<div aria-hidden="true" className="-my-1 flex flex-col items-center">
						<span className="size-2 rounded-full bg-border-strong" />
						<span className="h-7 w-px bg-gradient-to-b from-border-strong to-live" />
					</div>

					{/* The pipeline after the call */}
					<div className="tile p-0">
						<div className="flex items-center justify-between border-border border-b px-4 py-2.5">
							<span className="font-medium text-[13px]">After the call</span>
							<span className="inline-flex items-center gap-1.5 font-mono text-[10px] text-live-text uppercase tracking-[0.08em]">
								<span className="size-1.5 animate-live-pulse rounded-full bg-live" />
								processing
							</span>
						</div>
						<ol>
							{pipeline.map((row) => (
								<li
									className="flex items-center justify-between gap-3 border-border border-t px-4 py-2.5 font-mono text-[11px] first:border-t-0"
									key={row.step}
								>
									<span className="inline-flex items-center gap-2.5 text-fg-muted">
										<span
											className={`size-1.5 rounded-full ${
												row.done
													? "bg-ok"
													: row.live
														? "animate-live-pulse bg-live"
														: "border border-border-strong"
											}`}
										/>
										{row.step}
									</span>
									<span className="truncate text-fg-dim">{row.detail}</span>
								</li>
							))}
						</ol>
					</div>
				</div>

				<div>
					<SectionHead
						className="mb-7"
						eyebrow="How it works"
						title="Record without joining. Process without uploading."
					>
						When another app opens your microphone, a small prompt asks whether
						to record. The other side of the call comes from the system audio,
						your side from the mic, kept as two lanes so Steno always knows who
						is “me” and who is “them”. No bot joins. Nobody is told.
					</SectionHead>
					<Checklist
						items={[
							"Zoom, Teams, Meet, FaceTime, Slack, phone calls, in person",
							"On-device transcription with Parakeet or Whisper",
							"Remembers voices: name a speaker once",
							"German, English and the Denglish in between",
						]}
					/>
				</div>
			</div>
		</section>
	);
}
