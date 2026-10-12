import type {
	MeetingDetailSnapshot,
	ProgressSnapshot,
} from "@/bridge/contract";
import { Card, ProgressBar } from "@/components/ui";
import { usePlatform } from "@/lib/platform";
import { format } from "./format";

export type ProgressEntry = ProgressSnapshot["entries"][number];

export interface ProcessingCardProps {
	state: MeetingDetailSnapshot["state"];
	entry: ProgressEntry | undefined;
}

/** The progress stage of a queued meeting whose run waits for a model download. */
export const MODELS_MISSING_STAGE = "modelsMissing";

/**
 * Where the pipeline is with this meeting while it is queued or processing:
 * the stage, the time left, a bar, and the privacy line. Before the first
 * progress event the bar is indeterminate and the title says so; a meeting
 * that waits for a model download says what to do instead.
 */
export function ProcessingCard({ state, entry }: ProcessingCardProps) {
	const { words } = usePlatform();
	const title = entry
		? entry.title
		: state === "queued"
			? "Waiting to process"
			: "Processing";
	const remaining =
		entry?.estimatedRemainingSeconds !== undefined
			? format.remaining(entry.estimatedRemainingSeconds)
			: state === "queued" && entry?.stage !== MODELS_MISSING_STAGE
				? "starts when the current meeting finishes"
				: undefined;
	return (
		<Card
			className="mb-6 flex flex-col gap-2.5"
			data-testid="processing-card"
			padding="lg"
		>
			<div className="flex items-baseline justify-between gap-3 text-sm">
				<span className="font-medium" data-testid="processing-stage">
					{title}
				</span>
				{remaining ? (
					<span
						className="text-muted-foreground"
						data-testid="processing-remaining"
					>
						{remaining}
					</span>
				) : null}
			</div>
			<ProgressBar
				aria-label={title}
				data-testid="processing-bar"
				max={1}
				value={entry ? entry.fraction : null}
			/>
			<p className="my-0 text-faint text-xs">
				Audio stays on this {words.computer}. This usually takes a minute or
				two.
			</p>
		</Card>
	);
}
