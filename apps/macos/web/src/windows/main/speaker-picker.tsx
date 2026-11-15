import { CheckIcon, PlayIcon, SquareIcon } from "lucide-react";
import { type KeyboardEvent, type RefObject, useState } from "react";
import type { SpeakerOption } from "@/bridge/contract";
import { send, useBridge } from "@/bridge/hooks";
import {
	Avatar,
	Button,
	notify,
	Popover,
	PopoverPopup,
	PopoverTrigger,
	Tooltip,
} from "@/components/ui";
import {
	SelectFailure,
	SpeakerNameField,
	useSpeakerSelect,
} from "./speaker-name-field";
import {
	canPlay,
	isMe,
	isNamed,
	isSuggested,
	type Speaker,
	suggestionReason,
	voiceLabel,
} from "./speaker-names";

export interface SpeakerPlayButtonProps {
	speaker: Speaker;
	/** Suffix of the test id, so two hosts on one page stay apart. */
	testID: string;
	/** -1 inside a row that is one tab stop (its P key plays). */
	tabIndex?: number;
	className?: string;
}

/** Plays or stops the voice's sample clip, or says there is none. */
export function togglePlay(
	client: ReturnType<typeof useBridge>,
	speaker: Speaker,
) {
	if (!canPlay(speaker)) {
		return;
	}
	if (speaker.isPlaying) {
		send(client, "speakers.stop");
	} else {
		send(client, "speakers.play", { speakerID: speaker.id });
	}
}

/**
 * Plays or stops the voice's sample clip (`speakers.play`,
 * `speakers.stop`). A voice without a clip keeps the slot as a disabled
 * play labelled "No clip", so rows line up and the missing sound is said;
 * Me has no slot, the mic is the user's own voice.
 */
export function SpeakerPlayButton({
	speaker,
	testID,
	tabIndex,
	className,
}: SpeakerPlayButtonProps) {
	const client = useBridge();
	if (isMe(speaker)) {
		return null;
	}
	const playable = canPlay(speaker);
	const label = !playable
		? "No clip"
		: speaker.isPlaying
			? `Stop the sample of ${speaker.clusterLabel}`
			: `Play a sample of ${speaker.clusterLabel}`;
	return (
		<Button
			aria-label={label}
			{...(className ? { className } : {})}
			data-testid={testID}
			disabled={!playable}
			onClick={(event) => {
				event.stopPropagation();
				togglePlay(client, speaker);
			}}
			size="icon-xs"
			{...(tabIndex === undefined ? {} : { tabIndex })}
			title={playable ? undefined : "No clip"}
			variant="ghost"
		>
			{speaker.isPlaying ? (
				<SquareIcon aria-hidden="true" />
			) : (
				<PlayIcon aria-hidden="true" />
			)}
		</Button>
	);
}

/** "Speaker 1 is now Anna." once the host took a pick. */
export function announceNamed(speaker: Speaker, name: string) {
	notify({ title: `${voiceLabel(speaker)} is now ${name}.` });
}

export interface SpeakerPickerProps {
	speaker: Speaker;
	speakers: readonly Speaker[];
	open: boolean;
	onOpenChange: (open: boolean) => void;
	/** The turn the popup sits under, so its paragraph stays in view. */
	anchor: RefObject<HTMLElement | null>;
	/** The transcript's names are one tab stop; the arrows move between them. */
	tabIndex: number;
	onKeyDown: (event: KeyboardEvent<HTMLButtonElement>) => void;
	onFocus: () => void;
	triggerRef: (element: HTMLButtonElement | null) => void;
}

/**
 * A voice's name on a transcript turn, which opens a small panel under the
 * turn's paragraph: the voice, its clip, Yes for a suggestion, and the
 * same field as the Speakers panel. A pick names the voice in this meeting,
 * so every turn of it follows (all of them, until paragraphs can move on
 * their own); the result is announced and focus goes back to the name. A
 * suggestion carries a "?" and says why it is offered.
 */
export function SpeakerPicker({
	speaker,
	speakers,
	open,
	onOpenChange,
	anchor,
	tabIndex,
	onKeyDown,
	onFocus,
	triggerRef,
}: SpeakerPickerProps) {
	const { select, asking, dialog } = useSpeakerSelect();
	const [saving, setSaving] = useState<string | null>(null);
	const [failure, setFailure] = useState<{
		message: string;
		option: SpeakerOption;
		text: string;
	} | null>(null);
	const named = isNamed(speaker);
	const suggested = isSuggested(speaker);
	const tooltip = suggested
		? `${suggestionReason(speaker)}. Click to confirm or change`
		: named
			? "Click to change"
			: "Click to name";

	function change(next: boolean) {
		if (!next && (asking || saving !== null)) {
			return;
		}
		if (!next) {
			setFailure(null);
		}
		onOpenChange(next);
	}

	async function commit(option: SpeakerOption, text: string) {
		setFailure(null);
		setSaving(option.label);
		const outcome = await select(speaker, option);
		setSaving(null);
		if (outcome.kind === "committed") {
			announceNamed(speaker, option.label);
			onOpenChange(false);
		} else if (outcome.kind === "failed") {
			setFailure({ message: outcome.message, option, text });
		}
	}

	function confirmSuggestion() {
		if (speaker.personID) {
			void commit(
				{
					kind: "person",
					label: speaker.suggestionName ?? speaker.displayName,
					personID: speaker.personID,
				},
				"",
			);
		}
	}

	return (
		<Popover modal={false} onOpenChange={change} open={open}>
			<Tooltip label={tooltip}>
				<PopoverTrigger
					aria-label={
						suggested
							? `${speaker.displayName}, suggested. ${tooltip}`
							: speaker.displayName
					}
					className="-ml-[7px] min-w-0 max-w-full"
					data-testid={`speaker-picker-${speaker.id}`}
					onFocus={onFocus}
					onKeyDown={onKeyDown}
					ref={triggerRef}
					render={<Button size="name" variant="ghost" />}
					tabIndex={tabIndex}
				>
					<span className="truncate" title={speaker.displayName}>
						{speaker.displayName}
					</span>
					{suggested ? (
						<span
							aria-hidden="true"
							className="shrink-0 font-normal text-muted-foreground"
							data-testid={`speaker-suggested-${speaker.id}`}
						>
							?
						</span>
					) : null}
				</PopoverTrigger>
			</Tooltip>
			<PopoverPopup
				align="start"
				anchor={anchor}
				// Under the paragraph, or over the name when there is more
				// room there, never beside it in the reading column.
				collisionAvoidance={{ side: "flip", fallbackAxisSide: "none" }}
				data-testid={`speaker-panel-${speaker.id}`}
				padding="sm"
				size="picker"
			>
				<div className="flex min-h-8 min-w-0 items-center gap-2">
					<Avatar
						index={speaker.colorIndex}
						name={speaker.displayName}
						size="md"
						unknown={speaker.assignment === "unknown"}
					/>
					<span className="min-w-0 flex-1 truncate font-medium text-title">
						{named ? speaker.displayName : voiceLabel(speaker)}
						{named && isMe(speaker) ? (
							<span className="font-normal text-muted-foreground"> You</span>
						) : null}
					</span>
					<SpeakerPlayButton
						speaker={speaker}
						testID={`speaker-panel-play-${speaker.id}`}
					/>
				</div>
				{suggested && saving === null ? (
					<div className="mt-1 flex min-w-0 items-center gap-2">
						<span className="min-w-0 flex-1 truncate text-meta text-muted-foreground">
							{suggestionReason(speaker)}
						</span>
						<Button
							data-testid={`speaker-panel-yes-${speaker.id}`}
							onClick={confirmSuggestion}
							size="xs"
							variant="outline"
						>
							<CheckIcon aria-hidden="true" />
							Yes
						</Button>
					</div>
				) : null}
				{saving !== null ? (
					<p className="m-0 mt-2 flex min-w-0 items-baseline gap-2 text-body">
						<span className="min-w-0 truncate">{saving}</span>
						<span
							className="shrink-0 text-meta text-muted-foreground"
							data-testid="speaker-saving"
						>
							Saving
						</span>
					</p>
				) : open ? (
					<SpeakerNameField
						className="mt-2"
						initialText={failure?.text ?? ""}
						key={failure ? `failed-${failure.message}` : "field"}
						listClassName="max-h-56"
						notice={
							failure ? (
								<SelectFailure
									className="mt-1.5"
									message={failure.message}
									onRetry={() => void commit(failure.option, failure.text)}
									testID="speaker-panel-failure"
								/>
							) : null
						}
						onCancel={() => change(false)}
						onSubmit={(option, text) => void commit(option, text)}
						speaker={speaker}
						speakers={speakers}
					/>
				) : null}
				{dialog}
			</PopoverPopup>
		</Popover>
	);
}
