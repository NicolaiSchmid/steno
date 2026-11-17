import { CheckIcon, PlayIcon, SquareIcon } from "lucide-react";
import { type KeyboardEvent, type RefObject, useState } from "react";
import type { SpeakerOption } from "@/bridge/contract";
import { send, useBridge } from "@/bridge/hooks";
import {
	Avatar,
	Button,
	Kbd,
	notify,
	Popover,
	PopoverPopup,
	PopoverTrigger,
	Tooltip,
} from "@/components/ui";
import { cn } from "@/lib/cn";
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
	/** Says "Voice sample" beside the icon: the clip is the voice's, not the turn's. */
	labelled?: boolean;
	/** Shows "P" while keyboard focus is on the row around it (`group/row`). */
	keyHint?: boolean;
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
 * play with a muted "No sample" beside it, so rows line up and the missing
 * sound is said on screen; Me has no slot, the mic is the user's own voice.
 */
export function SpeakerPlayButton({
	speaker,
	testID,
	tabIndex,
	labelled = false,
	keyHint = false,
	className,
}: SpeakerPlayButtonProps) {
	const client = useBridge();
	if (isMe(speaker)) {
		return null;
	}
	const playable = canPlay(speaker);
	const label = !playable
		? "No sample"
		: speaker.isPlaying
			? `Stop the voice sample of ${speaker.clusterLabel}`
			: `Play the voice sample of ${speaker.clusterLabel}`;
	return (
		<span className={cn("flex shrink-0 items-center gap-1.5", className)}>
			{!playable ? (
				<span
					aria-hidden="true"
					className="text-meta text-muted-foreground"
					data-testid={`${testID}-none`}
				>
					No sample
				</span>
			) : keyHint ? (
				<Kbd
					aria-hidden="true"
					className="hidden group-focus-visible/row:inline-flex"
					variant="cap"
				>
					P
				</Kbd>
			) : null}
			<Button
				aria-label={label}
				data-testid={testID}
				disabled={!playable}
				onClick={(event) => {
					event.stopPropagation();
					togglePlay(client, speaker);
				}}
				size={labelled && playable ? "xs" : "icon-xs"}
				{...(tabIndex === undefined ? {} : { tabIndex })}
				variant="ghost"
			>
				{speaker.isPlaying ? (
					<SquareIcon aria-hidden="true" />
				) : (
					<PlayIcon aria-hidden="true" />
				)}
				{labelled && playable ? "Voice sample" : null}
			</Button>
		</span>
	);
}

/** "Speaker 1 is now Anna.", "You are Nicolai.", once the host took a pick. */
export function announceNamed(speaker: Speaker, name: string) {
	notify({
		title: isMe(speaker)
			? `You are ${name}.`
			: `${voiceLabel(speaker)} is now ${name}.`,
	});
}

/**
 * What a transcript pick changes: every paragraph of the voice in this
 * meeting, until paragraphs can move on their own. "Changes all 4
 * paragraphs of Speaker 3 in this meeting.", the person named too once the
 * voice is confirmed ("of Nicolai (Speaker 3)"); for one paragraph
 * "Changes Speaker 3’s only paragraph in this meeting."
 */
export function scopeLine(speaker: Speaker, paragraphs: number): string {
	const named = isNamed(speaker);
	if (isMe(speaker)) {
		return paragraphs === 1
			? "Changes your only paragraph in this meeting."
			: `Changes all ${paragraphs} paragraphs ${named ? `of ${speaker.displayName} (you)` : "of yours"} in this meeting.`;
	}
	if (paragraphs === 1) {
		return named
			? `Changes ${speaker.displayName}’s only paragraph (${speaker.clusterLabel}) in this meeting.`
			: `Changes ${speaker.clusterLabel}’s only paragraph in this meeting.`;
	}
	const whose = named
		? `${speaker.displayName} (${speaker.clusterLabel})`
		: speaker.clusterLabel;
	return `Changes all ${paragraphs} paragraphs of ${whose} in this meeting.`;
}

export interface SpeakerPickerProps {
	speaker: Speaker;
	speakers: readonly Speaker[];
	/** How many of the meeting's paragraphs the voice leads, for the scope line. */
	paragraphs: number;
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
 * their own), and the line above the field says so; the result is
 * announced and focus goes back to the name. A suggestion carries the grey
 * "?" bubble of the pill and the avatars, and says why it is offered.
 */
export function SpeakerPicker({
	speaker,
	speakers,
	paragraphs,
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
	// What the field reopens with after a cancelled combine or a failure.
	const [draft, setDraft] = useState({ text: "", attempt: 0 });
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
			setDraft({ text: "", attempt: 0 });
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
		} else {
			setDraft((now) => ({ text, attempt: now.attempt + 1 }));
			if (outcome.kind === "failed") {
				setFailure({ message: outcome.message, option, text });
			}
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
					className="-ml-[7px] min-w-0 max-w-full shrink"
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
						<Avatar
							aria-hidden="true"
							data-testid={`speaker-suggested-${speaker.id}`}
							size="sm"
							unknown
						/>
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
						labelled
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
					<>
						{/* What the pick changes, read before typing it. */}
						<p
							className="m-0 mt-2 text-meta text-muted-foreground"
							data-testid={`speaker-panel-scope-${speaker.id}`}
						>
							{scopeLine(speaker, paragraphs)}
						</p>
						<SpeakerNameField
							className="mt-1.5"
							initialText={draft.text}
							key={draft.attempt}
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
					</>
				) : null}
				{dialog}
			</PopoverPopup>
		</Popover>
	);
}
