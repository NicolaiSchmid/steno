import { CheckIcon, CopyIcon, MessageSquareTextIcon } from "lucide-react";
import { useEffect, useState } from "react";
import type { MeetingDetailSnapshot } from "@/bridge/contract";
import { Button, EmptyState } from "@/components/ui";
import { format } from "./format";
import {
	type Speaker,
	SpeakerPicker,
	SpeakerPlayButton,
} from "./speaker-picker";

export type Turn = MeetingDetailSnapshot["transcript"][number];

export interface TranscriptTabProps {
	detail: MeetingDetailSnapshot;
	/** Opens the picker on this speaker's first turn on mount (the screens). */
	initialPickerSpeakerID?: string | undefined;
}

/** The transcript as text: one line per turn, `Name: words`. */
export function transcriptText(turns: readonly Turn[]): string {
	return turns.map((turn) => `${turn.speakerName}: ${turn.text}`).join("\n");
}

function firstTurnOf(
	turns: readonly Turn[],
	speakerID: string,
): string | undefined {
	return turns.find((turn) => turn.speakerID === speakerID)?.id;
}

/**
 * The turns in two columns: who and when on the left, the words on the
 * right. A speaker's name opens the picker, named or not, so a wrong name
 * can be taken back where it is read; a speaker with a clip gets a play
 * button (Me never has one).
 */
export function TranscriptTab({
	detail,
	initialPickerSpeakerID,
}: TranscriptTabProps) {
	const [openTurnID, setOpenTurnID] = useState<string | null>(() =>
		initialPickerSpeakerID
			? (firstTurnOf(detail.transcript, initialPickerSpeakerID) ?? null)
			: null,
	);
	const [copied, setCopied] = useState(false);

	useEffect(() => {
		if (!copied) {
			return;
		}
		const timer = setTimeout(() => setCopied(false), 1500);
		return () => clearTimeout(timer);
	}, [copied]);

	const speakers = new Map<string, Speaker>(
		detail.speakers.map((speaker) => [speaker.id, speaker]),
	);

	if (detail.transcript.length === 0) {
		return (
			<EmptyState
				body="No speech was recognised."
				icon={<MessageSquareTextIcon aria-hidden="true" />}
				id="empty-transcript"
				title="No transcript"
			/>
		);
	}

	function copy() {
		const text = transcriptText(detail.transcript);
		navigator.clipboard
			?.writeText(text)
			.then(() => setCopied(true))
			.catch((cause: unknown) => {
				console.error("copy transcript failed", cause);
			});
	}

	return (
		<div data-testid="tab-content-transcript">
			<div className="mb-6 flex items-center gap-4 text-muted-foreground text-sm">
				<Button
					data-testid="copy-transcript"
					onClick={copy}
					size="xs"
					variant="ghost"
				>
					{copied ? (
						<CheckIcon aria-hidden="true" />
					) : (
						<CopyIcon aria-hidden="true" />
					)}
					{copied ? "Copied" : "Copy transcript"}
				</Button>
			</div>
			{detail.transcript.map((turn) => {
				const speaker = turn.speakerID
					? speakers.get(turn.speakerID)
					: undefined;
				return (
					<div className="mb-5 grid grid-cols-[140px_1fr] gap-4" key={turn.id}>
						<div className="flex flex-col gap-0.5 pt-0.5 font-medium text-sm">
							{speaker ? (
								<SpeakerPicker
									onOpenChange={(open) => setOpenTurnID(open ? turn.id : null)}
									open={openTurnID === turn.id}
									speaker={speaker}
								/>
							) : (
								<span className="truncate">{turn.speakerName}</span>
							)}
							<span className="font-mono font-normal text-2xs text-faint tabular-nums">
								{format.range(turn.startSeconds, turn.endSeconds)}
							</span>
							{speaker ? (
								<SpeakerPlayButton
									className="mt-1 -ml-[7px] self-start"
									speaker={speaker}
									testID={`speaker-play-${speaker.id}`}
								/>
							) : null}
						</div>
						<p className="my-0">{turn.text}</p>
					</div>
				);
			})}
		</div>
	);
}
