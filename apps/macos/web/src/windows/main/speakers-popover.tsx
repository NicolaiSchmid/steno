import {
	CheckIcon,
	ChevronDownIcon,
	InfoIcon,
	PencilIcon,
	UserPlusIcon,
} from "lucide-react";
import { useState } from "react";
import type { MeetingDetailSnapshot } from "@/bridge/contract";
import { send, useBridge } from "@/bridge/hooks";
import {
	Avatar,
	AvatarStack,
	Badge,
	Button,
	Popover,
	PopoverPopup,
	PopoverTitle,
	PopoverTrigger,
	Tooltip,
} from "@/components/ui";
import { cn } from "@/lib/cn";
import { SpeakerNameField } from "./speaker-name-field";
import {
	firstName,
	groupSpeakers,
	isMe,
	isNamed,
	nameAction,
	nextToName,
	type Speaker,
	type SpeakerGroups,
	snippet,
	type Turn,
} from "./speaker-names";
import { SpeakerPlayButton } from "./speaker-picker";

export const SPLIT_HINT =
	"Sometimes one person is heard as two speakers. Giving both the same name joins them into one.";

export interface SpeakersPopoverProps {
	speakers: MeetingDetailSnapshot["speakers"];
	transcript: MeetingDetailSnapshot["transcript"];
}

function needsLabel(count: number): string {
	return count === 1 ? "1 needs a name" : `${count} need names`;
}

/** The header's one entry point to naming, in three states. */
function TriggerContent({
	groups,
	total,
}: {
	groups: SpeakerGroups;
	total: number;
}) {
	if (groups.namedCount === 0) {
		return (
			<>
				<UserPlusIcon aria-hidden="true" />
				Name speakers
			</>
		);
	}
	const missing = total - groups.namedCount;
	return (
		<>
			<AvatarStack ring="background">
				{groups.named.map(({ speaker }) => (
					<Avatar
						index={speaker.colorIndex}
						key={speaker.id}
						name={speaker.displayName}
						size="md"
					/>
				))}
				{missing > 0 ? <Avatar name="Unnamed" size="md" unknown /> : null}
			</AvatarStack>
			<span className="truncate">
				{missing > 0
					? needsLabel(missing)
					: groups.named
							.map(({ speaker }) => firstName(speaker.displayName))
							.join(", ")}
			</span>
			<ChevronDownIcon aria-hidden="true" />
		</>
	);
}

/**
 * The header's Speakers button and its popover. The button reads "Name
 * speakers" (dashed) while nobody is named, the named avatars and "N need
 * names" while some are, and the avatars and first names once all are. The
 * popover lists the named speakers under "Speakers" and the rest under
 * "Need names", each unnamed one with something they said and the play
 * button for its sample clip. Clicking a row (or Enter on it) turns its name
 * into a text field (`SpeakerNameField`); after a pick the next speaker who
 * needs a name gets the field, so a meeting is named by typing and Enter. A
 * voice-matched suggestion has its own Confirm button. Test ids carry a
 * `speakers-popover-` prefix so they never collide with the transcript's.
 */
export function SpeakersPopover({
	speakers,
	transcript,
}: SpeakersPopoverProps) {
	const [open, setOpen] = useState(false);
	const [editing, setEditing] = useState<string | null>(null);
	const groups = groupSpeakers(speakers);
	const total = speakers.length;

	function onOpenChange(next: boolean) {
		setOpen(next);
		if (!next) {
			setEditing(null);
		}
	}

	function done(speaker: Speaker, picked: boolean) {
		setEditing((current) => {
			if (current !== speaker.id) {
				return current;
			}
			// Naming moves on to whoever is next; a rename stays put.
			return picked && !isNamed(speaker)
				? (nextToName(speakers, speaker.id)?.id ?? null)
				: null;
		});
	}

	const row = (speaker: Speaker, extra: Partial<SpeakerRowProps> = {}) => (
		<SpeakerRow
			editing={editing === speaker.id}
			key={speaker.id}
			onDone={(picked) => done(speaker, picked)}
			onEdit={() => setEditing(speaker.id)}
			speaker={speaker}
			transcript={transcript}
			{...extra}
		/>
	);

	return (
		<Popover modal={false} onOpenChange={onOpenChange} open={open}>
			<PopoverTrigger
				className={cn(groups.namedCount > 0 && "-ml-[7px] min-w-0 max-w-full")}
				data-testid="speakers-trigger"
				render={
					<Button
						size="sm"
						variant={groups.namedCount === 0 ? "dashed" : "ghost-muted"}
					/>
				}
			>
				<TriggerContent groups={groups} total={total} />
			</PopoverTrigger>
			<PopoverPopup align="start" size="lg">
				<div className="flex items-center gap-2">
					<PopoverTitle>Speakers</PopoverTitle>
					{groups.namedCount < total ? (
						<Badge data-testid="speakers-named-count" size="sm">
							{groups.namedCount} of {total} named
						</Badge>
					) : null}
				</div>
				{groups.named.length > 0 ? (
					<ul className="my-0 mt-2 flex list-none flex-col gap-0.5 p-0">
						{groups.named.flatMap(({ speaker, also }) => [
							row(speaker),
							...also.map((other, index) =>
								row(other, { split: true, hint: index === 0 }),
							),
						])}
					</ul>
				) : null}
				{groups.unnamed.length > 0 ? (
					<>
						<h3 className="mt-4 mb-0 font-medium text-muted-foreground text-xs">
							Need names
						</h3>
						<ul className="my-0 mt-1.5 flex list-none flex-col gap-0.5 p-0">
							{groups.unnamed.map((speaker) => row(speaker))}
						</ul>
					</>
				) : null}
			</PopoverPopup>
		</Popover>
	);
}

interface SpeakerRowProps {
	speaker: Speaker;
	transcript: readonly Turn[];
	editing: boolean;
	onEdit: () => void;
	onDone: (picked: boolean) => void;
	/** A second speaker of a named person, indented and dimmed. */
	split?: boolean;
	/** Shows the info tooltip that explains `split`. */
	hint?: boolean;
}

/**
 * One speaker: the avatar, then the name (a named or suggested speaker) or
 * the dashed "Name Speaker 1" chip, the play button, and, for anyone not yet
 * named, a line they said. Editing swaps the name for the field.
 */
function SpeakerRow({
	speaker,
	transcript,
	editing,
	onEdit,
	onDone,
	split = false,
	hint = false,
}: SpeakerRowProps) {
	const client = useBridge();
	const [preview, setPreview] = useState<string | undefined>(undefined);
	const named = isNamed(speaker);
	const suggested = speaker.assignment === "suggested";
	const said = named ? "" : snippet(speaker, transcript);

	function confirmSuggestion() {
		if (!speaker.personID) {
			return;
		}
		send(client, "speakers.select", {
			speakerID: speaker.id,
			option: {
				kind: "person",
				label: speaker.displayName,
				personID: speaker.personID,
			},
		});
	}

	return (
		<li
			className={cn(
				"flex flex-col",
				split && "ml-8",
				split && !editing && "opacity-64",
			)}
			data-testid={`speakers-popover-row-${speaker.id}`}
		>
			<div
				className={
					editing
						? "flex min-w-0 items-start gap-2.5 py-1"
						: "flex min-h-9 min-w-0 items-center gap-2.5"
				}
			>
				<Avatar
					className={cn(editing && "mt-px")}
					index={speaker.colorIndex}
					name={preview ?? speaker.displayName}
					size="lg"
					unknown={!preview && speaker.assignment === "unknown"}
				/>
				{editing ? (
					<SpeakerNameField
						cancelOnBlur
						className="min-w-0 flex-1"
						onDone={onDone}
						onPreview={setPreview}
						speaker={speaker}
					/>
				) : named || suggested ? (
					<>
						<Button
							className="-ml-[9px] min-w-0 flex-1 justify-start"
							data-testid={`speakers-popover-name-${speaker.id}`}
							onClick={onEdit}
							size="sm"
							variant="ghost"
						>
							<span className="truncate">{speaker.displayName}</span>
							{named && isMe(speaker) ? (
								<span className="shrink-0 font-normal text-faint">You</span>
							) : named && speaker.email ? (
								<span className="truncate font-normal text-faint">
									{speaker.email}
								</span>
							) : null}
							{suggested && !split ? (
								<Badge className="shrink-0" size="sm" variant="warning">
									Suggested
								</Badge>
							) : null}
						</Button>
						{hint ? (
							<Tooltip label={SPLIT_HINT}>
								<Button
									aria-label={SPLIT_HINT}
									size="icon-xs"
									variant="ghost-muted"
								>
									<InfoIcon aria-hidden="true" />
								</Button>
							</Tooltip>
						) : null}
						{suggested ? (
							<>
								<Button
									data-testid={`speakers-popover-confirm-${speaker.id}`}
									onClick={confirmSuggestion}
									size="xs"
									variant="outline"
								>
									<CheckIcon aria-hidden="true" />
									Confirm
								</Button>
								<Tooltip label="Change">
									<Button
										aria-label={`Change ${speaker.displayName}`}
										onClick={onEdit}
										size="icon-xs"
										variant="ghost-muted"
									>
										<PencilIcon aria-hidden="true" />
									</Button>
								</Tooltip>
							</>
						) : null}
					</>
				) : (
					<span className="min-w-0 flex-1">
						<Button
							className="max-w-full"
							data-testid={`speakers-popover-name-${speaker.id}`}
							onClick={onEdit}
							size="xs"
							variant="dashed"
						>
							<span className="truncate">{nameAction(speaker)}</span>
						</Button>
					</span>
				)}
				{editing ? null : (
					<SpeakerPlayButton
						iconOnly
						speaker={speaker}
						testID={`speakers-popover-play-${speaker.id}`}
					/>
				)}
			</div>
			{said && !editing ? (
				<p
					className="my-0 mb-1 ml-[42px] line-clamp-2 text-muted-foreground text-xs"
					data-testid={`speakers-popover-snippet-${speaker.id}`}
				>
					“{said}”
				</p>
			) : null}
		</li>
	);
}
