import { ClockIcon, FileTextIcon, SparklesIcon } from "lucide-react";
import type { ReactNode } from "react";
import type { MeetingDetailSnapshot } from "@/bridge/contract";
import { Button, Callout, EmptyState } from "@/components/ui";
import { TaskCard } from "./tasks-tab";

export interface SummaryTabProps {
	detail: MeetingDetailSnapshot;
	/** Opens Settings at Summaries (the summary was skipped: nothing set up). */
	onSetUpSummaries: () => void;
	/** Re-runs the summary (skipped, but an endpoint exists now). */
	onRerunSummary: () => void;
}

/** "Fokus" → "Fokus."; a lead that already ends a sentence stays. */
function leadText(lead: string): string {
	return /[.:!?]$/.test(lead) ? lead : `${lead}.`;
}

function statusTitle(status: MeetingDetailSnapshot["summaryStatus"]): string {
	if (status.title) {
		return status.title;
	}
	switch (status.kind) {
		case "pending":
			return "The summary is on its way.";
		case "skippedUnconfigured":
			return "Summaries are off.";
		case "skippedRunnable":
			return "No summary yet.";
		case "present":
			return "";
	}
}

/**
 * The reading column of a ready meeting: the summary sections, the
 * decisions and the tasks as cards. A summary that is pending or skipped
 * renders as the callout with its action instead.
 */
export function SummaryTab({
	detail,
	onSetUpSummaries,
	onRerunSummary,
}: SummaryTabProps) {
	const status = detail.summaryStatus;
	if (status.kind !== "present") {
		const pending = status.kind === "pending";
		const action =
			status.kind === "skippedUnconfigured"
				? onSetUpSummaries
				: status.kind === "skippedRunnable"
					? onRerunSummary
					: undefined;
		return (
			<Callout
				actions={
					action ? (
						<Button
							data-testid="summary-status-action"
							disabled={detail.isBusy}
							onClick={action}
							size="sm"
							variant="primary"
						>
							{status.actionTitle ??
								(status.kind === "skippedUnconfigured"
									? "Set up summaries"
									: "Write summary")}
						</Button>
					) : undefined
				}
				data-testid="summary-status"
				description={status.body}
				icon={
					pending ? (
						<ClockIcon aria-hidden="true" />
					) : (
						<SparklesIcon aria-hidden="true" />
					)
				}
				title={statusTitle(status)}
				variant={pending ? "info" : "warning"}
			/>
		);
	}

	const empty =
		detail.summary.length === 0 &&
		detail.decisions.length === 0 &&
		detail.tasks.length === 0;
	if (empty) {
		return (
			<EmptyState
				body="The template produced no sections."
				icon={<FileTextIcon aria-hidden="true" />}
				id="empty-summary"
				title="No summary"
			/>
		);
	}

	return (
		<div data-testid="tab-content-summary">
			{detail.summary.map((section) => (
				<Section key={section.id} title={section.heading}>
					<ul className="my-0 list-disc pl-[22px]">
						{section.bullets.map((bullet) => (
							<li className="my-1.5" key={`${bullet.lead}:${bullet.text}`}>
								<b className="font-semibold">{leadText(bullet.lead)}</b>{" "}
								{bullet.text}
							</li>
						))}
					</ul>
				</Section>
			))}
			{detail.decisions.length > 0 ? (
				<Section title="Decisions">
					<ul className="my-0 list-disc pl-[22px]">
						{detail.decisions.map((decision) => (
							<li className="my-1.5" key={decision}>
								{decision}
							</li>
						))}
					</ul>
				</Section>
			) : null}
			{detail.tasks.length > 0 ? (
				<Section title="Tasks">
					{detail.tasks.map((task) => (
						<TaskCard key={task.id} task={task} />
					))}
				</Section>
			) : null}
		</div>
	);
}

/** A heading and its content; the first section sits flush with the top. */
function Section({ title, children }: { title: string; children: ReactNode }) {
	return (
		<>
			<h3 className="mt-[26px] mb-2 font-semibold text-base first:mt-0">
				{title}
			</h3>
			{children}
		</>
	);
}
