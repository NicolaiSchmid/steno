import { CheckIcon, PlayIcon, SquareIcon } from "lucide-react";
import { type KeyboardEvent, type RefObject, useState } from "react";
import type { SpeakerOption } from "@/bridge/contract";
import { send, useBridge } from "@/bridge/hooks";
import { BridgeError } from "@/bridge/transport";
import {
	Avatar,
	Button,
	Kbd,
	notify,
	Popover,
	PopoverPopup,
	PopoverTrigger,
	ResultLine,
	Segmented,
	Tooltip,
} from "@/components/ui";
import { cn } from "@/lib/cn";
import {
	type FieldAction,
	SelectFailure,
	SpeakerNameField,
	selectSpeaker,
} from "./speaker-name-field";
import {
	allScopeLabel,
	canPlay,
	defaultScope,
	isDismissed,
	isMe,
	isNamed,
	isSuggested,
	NOT_A_PERSON,
	type PickScope,
	type Speaker,
	STALE_MOVE,
	suggestionReason,
	TURN_SCOPE_LINE,
	type Turn,
	turnSegments,
	type Voice,
	voiceLabel,
} from "./speaker-names";

export interface SpeakerPlayButtonProps {
	/** A speaker, or a voice combined into one. */
	speaker: Voice;
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
	speaker: Voice,
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

/** The command behind each field action. */
const ACTION_METHODS = {
	dismiss: "speakers.dismiss",
	clear: "speakers.clear",
	restore: "speakers.restore",
} as const;

/** A failed call's message, for the red line. */
function failureMessage(method: string, cause: unknown): string {
	console.error(`bridge: ${method} failed`, cause);
	return cause instanceof Error ? cause.message : String(cause);
}

/**
 * No to the suggestion that the voice is `personID` (`speakers.reject`):
 * that person is never suggested for it again. Resolves with the failure's
 * message, or null once the host took it.
 */
export async function sendReject(
	client: ReturnType<typeof useBridge>,
	speakerID: string,
	personID: string,
): Promise<string | null> {
	try {
		await client.call("speakers.reject", { speakerID, personID });
		return null;
	} catch (cause) {
		return failureMessage("speakers.reject", cause);
	}
}

/**
 * Not a person, Remove name or Restore. Resolves with the failure's
 * message, or null once the host took it.
 */
export async function sendAction(
	client: ReturnType<typeof useBridge>,
	speakerID: string,
	action: FieldAction,
): Promise<string | null> {
	const method = ACTION_METHODS[action];
	try {
		await client.call(method, { speakerID });
		return null;
	} catch (cause) {
		return failureMessage(method, cause);
	}
}

/**
 * "Speaker 4 is not a person.", "Speaker 2 has no name now.", "You have no
 * name in this meeting now.", "Speaker 4 is back among the speakers.",
 * once the host took the action.
 */
export function announceAction(speaker: Speaker, action: FieldAction) {
	const who = voiceLabel(speaker);
	const title =
		action === "dismiss"
			? `${who} is not a person.`
			: action === "restore"
				? `${who} is back among the speakers.`
				: isMe(speaker)
					? "You have no name in this meeting now."
					: `${who} has no name now.`;
	notify({ title });
}

/** A refused Separate again: the host's words, and whether to offer Retry. */
export interface SeparateFailure {
	message: string;
	/** Refused as `stale` (its paragraphs moved since): Retry cannot help. */
	stale: boolean;
}

/**
 * Undo after a combine and Separate again (`speakers.separate`): the voice
 * is its own again. Resolves with the failure, or null once the host took it.
 */
export async function sendSeparate(
	client: ReturnType<typeof useBridge>,
	speakerID: string,
): Promise<SeparateFailure | null> {
	try {
		await client.call("speakers.separate", { speakerID });
		return null;
	} catch (cause) {
		return {
			message: failureMessage("speakers.separate", cause),
			stale: cause instanceof BridgeError && cause.code === "stale",
		};
	}
}

/**
 * "Speaker 3 combined with Nicolai." once the host combined a pick, with
 * Undo, which separates the voice again; a failed Undo says so in a toast.
 */
export function announceCombined(
	client: ReturnType<typeof useBridge>,
	speaker: Speaker,
	name: string,
) {
	const who = isMe(speaker) ? "Your voice" : speaker.clusterLabel;
	notify({
		title: `${who} combined with ${name}.`,
		action: {
			label: "Undo",
			onClick: () => {
				void sendSeparate(client, speaker.id).then((failure) => {
					if (failure !== null) {
						notify({
							title: `${who} is still combined with ${name}.`,
							description: failure.message,
							tone: "destructive",
						});
					}
				});
			},
		},
	});
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
 * What a transcript pick for All changes: every paragraph of the voice in
 * this meeting. "Changes all 4
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

/**
 * The turn's own audio (`transcript.playTurn` over its segments, at most a
 * minute; `speakers.stop` stops it), "Paragraph" beside the icon. Once the
 * recording is gone it keeps the slot as a disabled play with a muted "No
 * recording", as a voice without a clip says "No sample".
 */
function TurnPlayButton({
	turn,
	meetingID,
	recording,
	onFailure,
}: {
	turn: Turn;
	meetingID: string;
	recording: boolean;
	/** The host could not play it ("The recording is gone."), or null. */
	onFailure: (message: string | null) => void;
}) {
	const client = useBridge();
	if (!recording) {
		return (
			<span className="flex shrink-0 items-center gap-1.5">
				<span
					aria-hidden="true"
					className="text-meta text-muted-foreground"
					data-testid={`turn-play-${turn.id}-none`}
				>
					No recording
				</span>
				<Button
					aria-label="No recording"
					data-testid={`turn-play-${turn.id}`}
					disabled
					size="icon-xs"
					variant="ghost"
				>
					<PlayIcon aria-hidden="true" />
				</Button>
			</span>
		);
	}
	async function toggle() {
		if (turn.isPlaying) {
			send(client, "speakers.stop");
			return;
		}
		onFailure(null);
		try {
			await client.call("transcript.playTurn", {
				meetingID,
				segmentIDs: turn.segmentIDs,
			});
		} catch (cause) {
			onFailure(failureMessage("transcript.playTurn", cause));
		}
	}
	return (
		<Button
			aria-label={
				turn.isPlaying ? "Stop this paragraph" : "Play this paragraph"
			}
			className="shrink-0"
			data-testid={`turn-play-${turn.id}`}
			onClick={() => void toggle()}
			size="xs"
			variant="ghost"
		>
			{turn.isPlaying ? (
				<SquareIcon aria-hidden="true" />
			) : (
				<PlayIcon aria-hidden="true" />
			)}
			Paragraph
		</Button>
	);
}

export interface SpeakerPickerProps {
	turn: Turn;
	/** The turn's voice; none for a turn without one. */
	speaker: Speaker | undefined;
	speakers: readonly Speaker[];
	/** How many of the meeting's paragraphs the voice leads, for All. */
	paragraphs: number;
	meetingID: string;
	/** The transcript's generation, which a move is checked against. */
	generation: number;
	/** The meeting's recording is still there, so the paragraph can play. */
	recording: boolean;
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

/** A failed pick, No or action, with what reopens the field. */
interface Failure {
	message: string;
	text: string;
	/** Sends it again; a stale move has Refresh instead. */
	retry: () => void;
	/** The host refused a move as `stale`: the line offers Refresh. */
	stale?: boolean;
}

/**
 * A turn's name, which opens a small panel under the turn's paragraph:
 * the voice, the paragraph's own audio, then the scope as a segmented
 * choice, This paragraph or All of the voice (preselected by
 * `defaultScope` each time the panel opens), the line saying what a pick
 * changes, and the same field as the Speakers panel. This paragraph moves
 * the turn's segments (`transcript.setTurnSpeaker`) against the transcript
 * as it was when the panel opened; the host refusing it as stale shows a
 * red line with Refresh, which takes the turn as it is now and keeps the
 * typed text. All names the voice in this meeting, and only All offers the
 * voice's own Yes and No and its action (Not a person, Remove name,
 * Restore). A turn without a voice has only This paragraph. A pick is
 * saved at once with "Saving"; the result is announced and focus goes back
 * to the name, a failure reopens the field with its text and a red line
 * with Retry. A suggestion carries the grey "?" bubble of the pill and the
 * avatars, and says why it is offered.
 */
export function SpeakerPicker({
	turn,
	speaker,
	speakers,
	paragraphs,
	meetingID,
	generation,
	recording,
	open,
	onOpenChange,
	anchor,
	tabIndex,
	onKeyDown,
	onFocus,
	triggerRef,
}: SpeakerPickerProps) {
	const client = useBridge();
	const [saving, setSaving] = useState<string | null>(null);
	// What the field reopens with after a failure.
	const [draft, setDraft] = useState({ text: "", attempt: 0 });
	const [failure, setFailure] = useState<Failure | null>(null);
	const [playFailure, setPlayFailure] = useState<string | null>(null);
	// Not remembered between turns: each opening starts on the default.
	const [chosen, setChosen] = useState<PickScope>(() => defaultScope(speaker));
	// The turn as it was when the panel opened: what a move is checked against.
	const [opened, setOpened] = useState(() => ({
		segments: turnSegments(turn),
		generation,
	}));
	// The person answered No to while the popup is open: the field leaves
	// them out until the options are asked for again.
	const [rejected, setRejected] = useState<string | null>(null);
	const scope: PickScope = speaker ? chosen : "turn";
	const named = speaker ? isNamed(speaker) : false;
	const suggested = speaker ? isSuggested(speaker) && rejected === null : false;
	const dismissed = speaker ? isDismissed(speaker) : false;
	const tooltip =
		suggested && speaker
			? `${suggestionReason(speaker)}. Click to confirm or change`
			: dismissed
				? `${NOT_A_PERSON}. Click to name or restore`
				: named
					? "Click to change"
					: "Click to name";
	const name = speaker ? speaker.displayName : turn.speakerName;
	const testID = speaker?.id ?? `turn-${turn.id}`;
	// Whom the field asks for options when the turn has no voice.
	const asked =
		speakers.find((other) => !isDismissed(other))?.id ?? speakers[0]?.id;

	function change(next: boolean) {
		if (!next && saving !== null) {
			return;
		}
		setFailure(null);
		setPlayFailure(null);
		setRejected(null);
		setDraft({ text: "", attempt: 0 });
		if (next) {
			setChosen(defaultScope(speaker));
			setOpened({ segments: turnSegments(turn), generation });
		}
		onOpenChange(next);
	}

	/** Fails the pick: the field reopens with its text and the red line. */
	function fail(next: Failure) {
		setDraft((now) => ({ text: next.text, attempt: now.attempt + 1 }));
		setFailure(next);
	}

	/** The turn as it is now, the typed text kept, the stale line gone. */
	function refresh() {
		setOpened({ segments: turnSegments(turn), generation });
		setFailure(null);
	}

	async function commit(option: SpeakerOption, text: string) {
		if (scope === "turn") {
			await move(option, text);
		} else if (speaker) {
			await nameVoice(speaker, option, text);
		}
	}

	/** This paragraph: "This paragraph is now Anna’s." */
	async function move(option: SpeakerOption, text: string) {
		setFailure(null);
		setSaving(option.label);
		try {
			await client.call("transcript.setTurnSpeaker", {
				meetingID,
				generation: opened.generation,
				segments: opened.segments,
				option,
			});
			setSaving(null);
			notify({ title: `This paragraph is now ${option.label}’s.` });
			onOpenChange(false);
		} catch (cause) {
			setSaving(null);
			if (cause instanceof BridgeError && cause.code === "stale") {
				console.error("bridge: transcript.setTurnSpeaker is stale", cause);
				fail({ message: STALE_MOVE, text, retry: refresh, stale: true });
			} else {
				fail({
					message: failureMessage("transcript.setTurnSpeaker", cause),
					text,
					retry: () => void commit(option, text),
				});
			}
		}
	}

	/** All of the voice: named, or combined with Undo. */
	async function nameVoice(
		voice: Speaker,
		option: SpeakerOption,
		text: string,
	) {
		setFailure(null);
		setSaving(option.label);
		const outcome = await selectSpeaker(client, voice, option);
		setSaving(null);
		if (outcome.kind === "failed") {
			fail({
				message: outcome.message,
				text,
				retry: () => void commit(option, text),
			});
			return;
		}
		if (outcome.kind === "combined") {
			announceCombined(client, voice, option.label);
		} else {
			announceNamed(voice, option.label);
		}
		onOpenChange(false);
	}

	/** No: the suggestion goes at once and the field offers everyone else. */
	async function reject() {
		const personID = speaker?.personID;
		if (!speaker || !personID) {
			return;
		}
		setFailure(null);
		setRejected(personID);
		const message = await sendReject(client, speaker.id, personID);
		if (message !== null) {
			setRejected(null);
			setFailure({ message, retry: () => void reject(), text: "" });
		}
	}

	/** Not a person, Remove name or Restore: announced, and the popup closes. */
	async function act(action: FieldAction) {
		if (!speaker) {
			return;
		}
		setFailure(null);
		setSaving(action === "dismiss" ? NOT_A_PERSON : speaker.clusterLabel);
		const message = await sendAction(client, speaker.id, action);
		setSaving(null);
		if (message === null) {
			announceAction(speaker, action);
			onOpenChange(false);
		} else {
			fail({ message, retry: () => void act(action), text: "" });
		}
	}

	function confirmSuggestion() {
		if (speaker?.personID) {
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

	const notice = failure ? (
		failure.stale ? (
			<ResultLine
				actions={
					<Button
						data-testid="speaker-panel-refresh"
						onClick={refresh}
						size="xs"
						variant="outline"
					>
						Refresh
					</Button>
				}
				className="mt-1.5"
				data-testid="speaker-panel-stale"
				tone="destructive"
			>
				{failure.message}
			</ResultLine>
		) : (
			<SelectFailure
				className="mt-1.5"
				message={failure.message}
				onRetry={failure.retry}
				testID="speaker-panel-failure"
			/>
		)
	) : null;

	return (
		<Popover modal={false} onOpenChange={change} open={open}>
			<Tooltip label={tooltip}>
				<PopoverTrigger
					aria-label={suggested ? `${name}, suggested. ${tooltip}` : name}
					className="-ml-[7px] min-w-0 max-w-full shrink"
					data-testid={`speaker-picker-${testID}`}
					onFocus={onFocus}
					onKeyDown={onKeyDown}
					ref={triggerRef}
					render={<Button size="name" variant="ghost" />}
					tabIndex={tabIndex}
				>
					<span
						className={cn("truncate", dismissed && "text-muted-foreground")}
						title={name}
					>
						{dismissed ? NOT_A_PERSON : name}
					</span>
					{suggested && speaker ? (
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
				data-testid={`speaker-panel-${testID}`}
				padding="sm"
				size="picker"
			>
				<div className="flex min-h-8 min-w-0 items-center gap-2">
					<Avatar
						{...(speaker ? { index: speaker.colorIndex } : {})}
						name={name}
						size="md"
						unknown={
							!speaker || speaker.assignment === "unknown" || rejected !== null
						}
					/>
					<span className="min-w-0 flex-1 truncate font-medium text-title">
						{!speaker
							? name
							: named
								? speaker.displayName
								: voiceLabel(speaker)}
						{dismissed ? (
							<span className="font-normal text-muted-foreground">
								{" "}
								{NOT_A_PERSON}
							</span>
						) : null}
						{named && speaker && isMe(speaker) ? (
							<span className="font-normal text-muted-foreground"> You</span>
						) : null}
					</span>
					<TurnPlayButton
						meetingID={meetingID}
						onFailure={setPlayFailure}
						recording={recording}
						turn={turn}
					/>
				</div>
				{playFailure ? (
					<ResultLine
						className="mt-1.5"
						data-testid="turn-play-failure"
						tone="destructive"
					>
						{playFailure}
					</ResultLine>
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
						{speaker ? (
							<Segmented
								aria-label="What the name changes"
								className="mt-2"
								data-testid={`speaker-panel-scope-choice-${testID}`}
								onValueChange={setChosen}
								options={[
									{
										value: "turn",
										label: "This paragraph",
										className: "shrink-0",
									},
									{
										value: "voice",
										label: allScopeLabel(speaker, paragraphs),
									},
								]}
								value={scope}
							/>
						) : null}
						{/* What the pick changes, read before typing it. */}
						<p
							className="m-0 mt-1.5 text-meta text-muted-foreground"
							data-testid={`speaker-panel-scope-${testID}`}
						>
							{scope === "turn" || !speaker
								? TURN_SCOPE_LINE
								: scopeLine(speaker, paragraphs)}
						</p>
						{scope === "voice" && suggested && speaker ? (
							<div className="mt-1.5 flex min-w-0 items-center gap-2">
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
								<Button
									data-testid={`speaker-panel-no-${speaker.id}`}
									onClick={() => void reject()}
									size="xs"
									variant="ghost"
								>
									No
								</Button>
							</div>
						) : null}
						<SpeakerNameField
							className="mt-1.5"
							{...(rejected && scope === "voice"
								? { excludedPersonID: rejected }
								: {})}
							initialText={draft.text}
							key={draft.attempt}
							listClassName="max-h-56"
							notice={notice}
							{...(scope === "voice"
								? { onAction: (action: FieldAction) => void act(action) }
								: {})}
							onCancel={() => change(false)}
							onSubmit={(option, text) => void commit(option, text)}
							optionsSpeakerID={asked}
							placeholder={turn.speakerName}
							scope={scope}
							speaker={speaker}
							speakers={speakers}
						/>
					</>
				) : null}
			</PopoverPopup>
		</Popover>
	);
}
