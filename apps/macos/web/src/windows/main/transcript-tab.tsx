import { MessageSquareTextIcon } from "lucide-react";
import { useState } from "react";
import type { MeetingDetailSnapshot } from "@/bridge/contract";
import { EmptyState } from "@/components/ui";
import { format } from "./format";
import type { Speaker, Turn } from "./speaker-names";
import { SpeakerPicker, SpeakerPlayButton } from "./speaker-picker";

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
 * The turns as paragraphs, each led by a line with the speaker's name in
 * bold, the turn's time in the meta size and the sample's play button, all
 * visible. A speaker's name opens the picker, named or not, so a wrong name
 * can be taken back where it is read; a speaker with a clip gets a play
 * button (Me never has one). Copying the transcript lives in the meeting's
 * actions menu.
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

	return (
		<div data-testid="tab-content-transcript">
			{detail.transcript.map((turn) => {
				const speaker = turn.speakerID
					? speakers.get(turn.speakerID)
					: undefined;
				return (
					<div className="mb-5" data-testid={`turn-${turn.id}`} key={turn.id}>
						<div className="flex h-6 min-w-0 items-center gap-2">
							{speaker ? (
								<SpeakerPicker
									onOpenChange={(open) => setOpenTurnID(open ? turn.id : null)}
									open={openTurnID === turn.id}
									speaker={speaker}
								/>
							) : (
								<span className="min-w-0 truncate font-semibold text-title">
									{turn.speakerName}
								</span>
							)}
							<span className="shrink-0 font-mono text-meta text-muted-foreground tabular-nums">
								{format.range(turn.startSeconds, turn.endSeconds)}
							</span>
							{speaker ? (
								<SpeakerPlayButton
									iconOnly
									speaker={speaker}
									testID={`speaker-play-${speaker.id}`}
								/>
							) : null}
						</div>
						<p className="my-0 mt-1">{turn.text}</p>
					</div>
				);
			})}
		</div>
	);
}
