import { MessageSquareTextIcon } from "lucide-react";
import { type KeyboardEvent, useRef, useState } from "react";
import type { MeetingDetailSnapshot } from "@/bridge/contract";
import { Avatar, EmptyState } from "@/components/ui";
import { format } from "./format";
import type { Speaker, Turn } from "./speaker-names";
import { SpeakerPicker } from "./speaker-picker";

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

const NEXT: Record<string, number> = { ArrowDown: 1, ArrowRight: 1 };
const PREVIOUS: Record<string, number> = { ArrowUp: -1, ArrowLeft: -1 };

/**
 * The turns as paragraphs, each led by a line with the speaker's avatar
 * (the pill's and the panel's colour, decoration only), their name in bold
 * and the turn's time. A voice's name opens its picker under the paragraph,
 * named or not, so a wrong name can be taken back where it is read; the
 * clip plays from there. The names are one tab stop: the arrow keys, Home
 * and End move between them. Copying the transcript lives in the meeting's
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
	const [current, setCurrent] = useState(0);
	const names = useRef<(HTMLButtonElement | null)[]>([]);

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

	const withNames = detail.transcript.filter(
		(turn) => turn.speakerID && speakers.has(turn.speakerID),
	);
	const focusable = Math.min(current, withNames.length - 1);

	function move(event: KeyboardEvent<HTMLButtonElement>, at: number) {
		const last = withNames.length - 1;
		const target =
			event.key in NEXT || event.key in PREVIOUS
				? Math.min(
						Math.max(at + (NEXT[event.key] ?? PREVIOUS[event.key] ?? 0), 0),
						last,
					)
				: event.key === "Home"
					? 0
					: event.key === "End"
						? last
						: undefined;
		if (target === undefined) {
			return;
		}
		event.preventDefault();
		setCurrent(target);
		names.current[target]?.focus();
	}

	const paragraphs = new Map<string, number>();
	for (const turn of detail.transcript) {
		if (turn.speakerID) {
			paragraphs.set(turn.speakerID, (paragraphs.get(turn.speakerID) ?? 0) + 1);
		}
	}

	return (
		<div data-testid="tab-content-transcript">
			{detail.transcript.map((turn) => {
				const speaker = turn.speakerID
					? speakers.get(turn.speakerID)
					: undefined;
				const at = withNames.indexOf(turn);
				return (
					<TurnView
						at={at}
						focusable={at === focusable}
						key={turn.id}
						names={names.current}
						onFocusName={() => setCurrent(at)}
						onMove={move}
						onOpenChange={(open) => setOpenTurnID(open ? turn.id : null)}
						open={openTurnID === turn.id}
						paragraphs={
							turn.speakerID ? (paragraphs.get(turn.speakerID) ?? 0) : 0
						}
						speaker={speaker}
						speakers={detail.speakers}
						turn={turn}
					/>
				);
			})}
		</div>
	);
}

interface TurnViewProps {
	turn: Turn;
	speaker: Speaker | undefined;
	speakers: readonly Speaker[];
	/** How many paragraphs the turn's voice leads in the meeting. */
	paragraphs: number;
	at: number;
	focusable: boolean;
	names: (HTMLButtonElement | null)[];
	open: boolean;
	onOpenChange: (open: boolean) => void;
	onFocusName: () => void;
	onMove: (event: KeyboardEvent<HTMLButtonElement>, at: number) => void;
}

function TurnView({
	turn,
	speaker,
	speakers,
	paragraphs,
	at,
	focusable,
	names,
	open,
	onOpenChange,
	onFocusName,
	onMove,
}: TurnViewProps) {
	const anchor = useRef<HTMLDivElement>(null);
	return (
		<div className="mb-5" data-testid={`turn-${turn.id}`} ref={anchor}>
			<div className="flex h-6 min-w-0 items-center gap-2">
				{speaker ? (
					<>
						<Avatar
							aria-hidden="true"
							index={speaker.colorIndex}
							name={speaker.displayName}
							size="sm"
							unknown={speaker.assignment === "unknown"}
						/>
						<SpeakerPicker
							anchor={anchor}
							onFocus={onFocusName}
							onKeyDown={(event) => onMove(event, at)}
							onOpenChange={onOpenChange}
							open={open}
							paragraphs={paragraphs}
							speaker={speaker}
							speakers={speakers}
							tabIndex={focusable ? 0 : -1}
							triggerRef={(element) => {
								names[at] = element;
							}}
						/>
					</>
				) : (
					<span
						className="min-w-0 truncate font-semibold text-title"
						title={turn.speakerName}
					>
						{turn.speakerName}
					</span>
				)}
				<span className="shrink-0 font-mono text-meta text-muted-foreground tabular-nums">
					{format.range(turn.startSeconds, turn.endSeconds)}
				</span>
			</div>
			<p className="my-0 mt-1">{turn.text}</p>
		</div>
	);
}
