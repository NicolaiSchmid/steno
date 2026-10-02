import {
	Calendar,
	ChevronRight,
	Clock,
	FileText,
	Folder,
	Inbox,
	ListChecks,
	Search,
	Settings,
	Users,
} from "lucide-react";
import { RecordMark } from "@/components/record-mark";
import { cn } from "@/lib/cn";

/**
 * The hero screenshot, built in HTML so it stays sharp and themeable: Steno's
 * main window with the README's example meeting. Nothing here is a real
 * recording.
 */

const sidebar: Array<{
	group: string;
	items: Array<{
		title: string;
		meta: string;
		selected?: boolean;
		live?: boolean;
		pending?: boolean;
	}>;
}> = [
	{
		group: "Today",
		items: [
			{ title: "Weekly mit Jérôme", meta: "Recording · 12:41", live: true },
			{
				title: "Produktstrategie: „90/10“ & Roadmap für Q4",
				meta: "14:00 · 1 h 30 min",
				selected: true,
			},
		],
	},
	{
		group: "Yesterday",
		items: [
			{ title: "Customer discovery · ACME", meta: "10:30 · 48 min" },
			{ title: "Standup", meta: "09:15 · 11 min" },
			{
				title: "Voice memo · Zugfahrt",
				meta: "From iPhone · processing",
				pending: true,
			},
		],
	},
	{
		group: "Last week",
		items: [
			{ title: "Interview · Backend Engineer", meta: "Thu · 55 min" },
			{ title: "Board prep", meta: "Wed · 1 h 05 min" },
			{ title: "Call with Anna (Continuity)", meta: "Tue · 22 min" },
		],
	},
];

const tabs = ["Summary", "Transcript", "Tasks", "Files"];

const people = [
	{ name: "Anna Müller", role: "matched · 0.91", tone: "text-p1" },
	{ name: "Jérôme Dupont", role: "new voice · name?", tone: "text-p2" },
	{ name: "Nicolai Schmid", role: "me · mic lane", tone: "text-p4" },
];

export function ProductFrame() {
	return (
		<div className="overflow-hidden rounded-[14px] border border-border bg-bg-elev shadow-frame">
			<div className="flex h-[560px] overflow-hidden text-left lg:h-[680px]">
				{/* Left rail */}
				<div className="hidden w-12 shrink-0 flex-col items-center gap-1 border-border border-r bg-white/[0.02] py-2 md:flex">
					{[Inbox, Search, Users, Calendar].map((Icon, i) => (
						<span
							className={cn(
								"grid size-8 place-items-center rounded-[6px]",
								i === 0 ? "bg-white/[0.06] text-fg" : "text-fg-dim",
							)}
							key={Icon.displayName ?? i}
						>
							<Icon className="size-4" strokeWidth={2} />
						</span>
					))}
					<span className="mt-auto grid size-8 place-items-center rounded-[6px] text-fg-dim">
						<Settings className="size-4" strokeWidth={2} />
					</span>
				</div>

				{/* Sidebar */}
				<div className="hidden w-[232px] shrink-0 flex-col border-border border-r bg-white/[0.02] sm:flex">
					<div className="flex items-center justify-between border-border border-b px-3 py-2.5">
						<span className="inline-flex items-center gap-2 font-medium text-[11px] text-fg">
							<RecordMark className="h-[10px]" />
							Meetings
						</span>
						<span className="rounded-xs border border-border px-1.5 font-mono text-[9px] text-fg-dim leading-4">
							⌘⇧R
						</span>
					</div>
					<div className="flex-1 overflow-hidden px-2 py-2">
						{sidebar.map((group) => (
							<div className="mb-3" key={group.group}>
								<div className="px-2 pb-1 font-medium text-[10px] text-fg-dim uppercase tracking-[0.1em]">
									{group.group}
								</div>
								{group.items.map((item) => (
									<div
										className={cn(
											"flex items-start gap-2 rounded-[6px] px-2 py-1.5",
											item.selected && "bg-white/[0.07]",
										)}
										key={item.title}
									>
										<span className="mt-1 size-1.5 shrink-0">
											{item.live ? (
												<span className="block size-1.5 animate-live-pulse rounded-full bg-live" />
											) : item.pending ? (
												<span className="block size-1.5 rounded-full border border-border-strong" />
											) : null}
										</span>
										<span className="min-w-0">
											<span
												className={cn(
													"block truncate text-[12px] leading-4",
													item.selected
														? "font-medium text-fg"
														: "text-fg-muted",
												)}
											>
												{item.title}
											</span>
											<span
												className={cn(
													"block text-[10px] leading-4",
													item.live ? "text-live-text" : "text-fg-dim",
												)}
											>
												{item.meta}
											</span>
										</span>
									</div>
								))}
							</div>
						))}
					</div>
				</div>

				{/* Main */}
				<div className="flex min-w-0 flex-1 flex-col">
					<div className="flex items-center gap-1.5 border-border border-b px-4 py-2.5 text-[11px] text-fg-dim sm:px-6">
						<Folder className="size-3" strokeWidth={2} />
						<span>Meetings</span>
						<ChevronRight className="size-3" strokeWidth={2} />
						<span>2026-09-24</span>
						<ChevronRight className="size-3" strokeWidth={2} />
						<span className="truncate text-fg-muted">
							produktstrategie-90-10-roadmap-fuer-q4
						</span>
						<span className="ml-auto inline-flex items-center gap-1.5 rounded-[6px] bg-ok-dim px-1.5 py-px font-medium text-[10px] text-ok">
							<span className="size-1.5 rounded-full bg-ok" />
							Saved
						</span>
					</div>

					<div className="px-4 pt-4 sm:px-6 sm:pt-5">
						<h3 className="font-semibold text-[16px] text-fg-muted leading-6 sm:text-lg sm:leading-7">
							Produktstrategie: „90/10“ &amp; Roadmap für Q4
						</h3>
						<div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-fg-dim">
							<span className="inline-flex items-center gap-1">
								<Calendar className="size-3" strokeWidth={2} />
								Wed 24 Sep · 14:00–15:30
							</span>
							<span className="inline-flex items-center gap-1">
								<Clock className="size-3" strokeWidth={2} />1 h 30 min
							</span>
							<span>Mac call · Zoom</span>
							<span>de · Default template</span>
							<span className="inline-flex items-center gap-1">
								<span className="flex -space-x-1">
									{people.map((p) => (
										<span
											className={cn(
												"grid size-4 place-items-center rounded-full border border-bg-elev bg-white/[0.06] font-medium text-[8px]",
												p.tone,
											)}
											key={p.name}
										>
											{p.name[0]}
										</span>
									))}
								</span>
								3 people
							</span>
						</div>
					</div>

					<div className="mt-3 flex gap-1 border-border border-y px-4 py-2 sm:px-6">
						{tabs.map((tab, i) => (
							<span
								className={cn(
									"rounded-[6px] px-2.5 py-1 text-[11px] leading-4",
									i === 0
										? "bg-white/[0.06] font-medium text-fg"
										: "text-fg-muted",
								)}
								key={tab}
							>
								{tab}
							</span>
						))}
					</div>

					<div className="flex min-h-0 flex-1">
						<div className="min-w-0 flex-1 overflow-hidden px-4 py-4 text-[12px] text-fg-muted leading-[1.625] sm:px-6 sm:py-6 sm:text-[13px]">
							<div className="max-w-xl space-y-4">
								<div>
									<div className="font-semibold text-[13px] text-fg">
										Executive summary
									</div>
									<ul className="mt-1.5 list-disc space-y-1 pl-4">
										<li>
											<b className="font-medium text-fg">Fokus:</b>{" "}
											<span className="text-p1">Anna Müller</span> setzt 90
											Prozent auf den Kern und 10 auf Experimente. Das Team
											zieht mit.
										</li>
										<li>
											Q4-Roadmap: Onboarding-Rewrite zuerst, die
											Enterprise-SSO-Anfrage von ACME wird nach dem Pricing-Call
											entschieden.
										</li>
										<li>
											<span className="text-p2">Jérôme</span> übernimmt das
											Angebot an ACME, Deadline 1. Oktober.
										</li>
									</ul>
								</div>
								<div>
									<div className="font-semibold text-[13px] text-fg">
										Decisions
									</div>
									<ul className="mt-1.5 list-disc space-y-1 pl-4">
										<li>90/10-Aufteilung wird umgesetzt.</li>
										<li>
											Kein neues Feature vor dem Onboarding-Rewrite, Ausnahme:
											Bugfixes für bezahlte Kunden.
										</li>
									</ul>
								</div>
								<div>
									<div className="flex items-center gap-1.5 font-semibold text-[13px] text-fg">
										<ListChecks className="size-3.5" strokeWidth={2} />
										Tasks
									</div>
									<ul className="mt-1.5 space-y-1 font-mono text-[11px] leading-[1.625] sm:text-[12px]">
										<li className="flex items-start gap-2">
											<span className="mt-1 size-3 shrink-0 rounded-xs border border-border-strong" />
											<span>
												Angebot an ACME schicken{" "}
												<span className="text-p2">[[Jérôme Dupont]]</span> ⏫ 📅
												2026-10-01
											</span>
										</li>
										<li className="flex items-start gap-2">
											<span className="mt-1 size-3 shrink-0 rounded-xs border border-border-strong" />
											<span>
												Roadmap-Folien aktualisieren{" "}
												<span className="text-p4">[[Nicolai Schmid]]</span> 📅
												2026-10-15
											</span>
										</li>
										<li className="flex items-start gap-2">
											<span className="mt-1 size-3 shrink-0 rounded-xs border border-border-strong" />
											<span>
												Pricing-Call mit ACME ansetzen{" "}
												<span className="text-p1">[[Anna Müller]]</span> 🔼 📅
												2026-10-08
											</span>
										</li>
									</ul>
								</div>
								<div className="rounded-sm border border-border bg-white/[0.02] p-3 text-[11px] text-fg-dim leading-4">
									<div className="flex items-center gap-1.5 text-fg-muted">
										<FileText className="size-3" strokeWidth={2} />
										Written to{" "}
										<code className="rounded-xs border border-border bg-white/[0.04] px-1 font-mono text-[10px]">
											Meetings/2026-09-24-produktstrategie-90-10-roadmap-fuer-q4/
										</code>
									</div>
									<div className="mt-1">
										Summary.md · Transcript.md · Tasks.md · transcript.vtt ·
										meeting.json · audio.m4a kept 30 days
									</div>
								</div>
							</div>
						</div>

						{/* Detail rail */}
						<div className="hidden w-[224px] shrink-0 border-border border-l bg-white/[0.02] p-4 lg:block">
							<div className="font-medium text-[10px] text-fg-dim uppercase tracking-[0.1em]">
								Speakers
							</div>
							<ul className="mt-2 divide-y divide-border">
								{people.map((p) => (
									<li className="flex items-center gap-2.5 py-2.5" key={p.name}>
										<span
											className={cn(
												"grid size-6 place-items-center rounded-full bg-white/[0.06] font-medium text-[10px]",
												p.tone,
											)}
										>
											{p.name[0]}
										</span>
										<span className="min-w-0">
											<span className="block truncate text-[12px] text-fg leading-4">
												{p.name}
											</span>
											<span className="block text-[10px] text-fg-dim leading-4">
												{p.role}
											</span>
										</span>
									</li>
								))}
							</ul>
							<div className="mt-5 font-medium text-[10px] text-fg-dim uppercase tracking-[0.1em]">
								Pipeline
							</div>
							<dl className="mt-2 divide-y divide-border text-[11px] leading-4">
								{[
									["Transcribe", "Parakeet TDT v3 · 1:42"],
									["Diarize", "3 speakers · 0:38"],
									["Cleanup", "Denglish → de · 0:21"],
									["Summary", "Default · 0:17"],
									["Export", "Markdown, VTT, JSON"],
								].map(([k, v]) => (
									<div className="flex justify-between gap-2 py-2" key={k}>
										<dt className="text-fg-dim">{k}</dt>
										<dd className="truncate text-right text-fg-muted">{v}</dd>
									</div>
								))}
							</dl>
						</div>
					</div>
				</div>
			</div>
		</div>
	);
}
