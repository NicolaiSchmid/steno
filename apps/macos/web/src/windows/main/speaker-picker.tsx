import { PlayIcon, SquareIcon } from "lucide-react";
import { send, useBridge } from "@/bridge/hooks";
import {
	Avatar,
	Button,
	Popover,
	PopoverPopup,
	PopoverTrigger,
	Tooltip,
} from "@/components/ui";
import { SpeakerNameField } from "./speaker-name-field";
import { canPlay, isMe, isNamed, type Speaker } from "./speaker-names";

export type { Speaker } from "./speaker-names";

export interface SpeakerPlayButtonProps {
	speaker: Speaker;
	/** Suffix of the test id, so two hosts on one page stay apart. */
	testID: string;
	/** Icon only, for a row; the transcript spells out Play and Stop. */
	iconOnly?: boolean;
	className?: string;
}

/**
 * Plays or stops the speaker's sample clip (`speakers.play`,
 * `speakers.stop`); renders nothing without a clip, and never for Me.
 */
export function SpeakerPlayButton({
	speaker,
	testID,
	iconOnly = false,
	className,
}: SpeakerPlayButtonProps) {
	const client = useBridge();
	if (!canPlay(speaker)) {
		return null;
	}
	function toggle() {
		if (speaker.isPlaying) {
			send(client, "speakers.stop");
		} else {
			send(client, "speakers.play", { speakerID: speaker.id });
		}
	}
	return (
		<Button
			aria-label={
				speaker.isPlaying
					? `Stop the sample of ${speaker.displayName}`
					: `Play a sample of ${speaker.displayName}`
			}
			{...(className ? { className } : {})}
			data-testid={testID}
			onClick={toggle}
			size={iconOnly ? "icon-xs" : "xs"}
			variant="ghost"
		>
			{speaker.isPlaying ? (
				<SquareIcon aria-hidden="true" />
			) : (
				<PlayIcon aria-hidden="true" />
			)}
			{iconOnly ? null : speaker.isPlaying ? "Stop" : "Play"}
		</Button>
	);
}

export interface SpeakerPickerProps {
	speaker: Speaker;
	open: boolean;
	onOpenChange: (open: boolean) => void;
}

/**
 * A speaker's name on a transcript turn: a button ("Click to rename") that
 * opens a small popover with the speaker, the sample clip's play button
 * and the same name field as the Speakers popover. A pick applies to every
 * turn of the speaker, since the host re-publishes the meeting.
 */
export function SpeakerPicker({
	speaker,
	open,
	onOpenChange,
}: SpeakerPickerProps) {
	const named = isNamed(speaker);
	return (
		<Popover modal={false} onOpenChange={onOpenChange} open={open}>
			<Tooltip label={named ? "Click to rename" : "Click to name"} side="left">
				<PopoverTrigger
					className="-ml-[7px] max-w-full self-start"
					data-testid={`speaker-picker-${speaker.id}`}
					render={<Button size="xs" variant="ghost" />}
				>
					<span className="truncate">{speaker.displayName}</span>
				</PopoverTrigger>
			</Tooltip>
			<PopoverPopup align="start" padding="sm" size="sm">
				<div className="mb-2 flex min-w-0 items-center gap-2">
					<Avatar
						index={speaker.colorIndex}
						name={speaker.displayName}
						size="md"
						unknown={!named && speaker.assignment === "unknown"}
					/>
					<span className="min-w-0 flex-1 truncate font-medium text-sm">
						{isMe(speaker) && !named ? "You" : speaker.displayName}
					</span>
					<SpeakerPlayButton
						iconOnly
						speaker={speaker}
						testID={`speaker-popover-play-${speaker.id}`}
					/>
				</div>
				{open ? (
					<SpeakerNameField
						onDone={() => onOpenChange(false)}
						speaker={speaker}
					/>
				) : null}
			</PopoverPopup>
		</Popover>
	);
}
