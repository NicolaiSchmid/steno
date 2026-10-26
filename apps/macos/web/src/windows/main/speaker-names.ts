import type { MeetingDetailSnapshot, SpeakerOption } from "@/bridge/contract";

export type Speaker = MeetingDetailSnapshot["speakers"][number];
export type Turn = MeetingDetailSnapshot["transcript"][number];

/**
 * The cluster label of the microphone's speaker, the person at this
 * computer (`ME_SPEAKER_LABEL` in `steno-pipeline`). It has no sample clip:
 * the mic is the user's own voice.
 */
export const ME_LABEL = "Me";

export function isMe(speaker: Speaker): boolean {
	return speaker.clusterLabel === ME_LABEL;
}

export function isNamed(speaker: Speaker): boolean {
	return speaker.assignment === "confirmed";
}

/** Whether the row may offer the sample clip: never for Me, else with a clip. */
export function canPlay(speaker: Speaker): boolean {
	return speaker.hasClip && !isMe(speaker);
}

/** What an unnamed speaker's chip asks for: "Name yourself" or "Name Speaker 1". */
export function nameAction(speaker: Speaker): string {
	return isMe(speaker) ? "Name yourself" : `Name ${speaker.clusterLabel}`;
}

export function firstName(name: string): string {
	return name.trim().split(/\s+/)[0] ?? name;
}

const SNIPPET_MIN_WORDS = 8;
const SNIPPET_MAX_LENGTH = 90;

/**
 * Something the speaker said, to tell who it is: their first turn of eight
 * words or more, else their longest, cut at a word with "…" past 90
 * characters. Empty when they have no turn.
 */
export function snippet(speaker: Speaker, turns: readonly Turn[]): string {
	const own = turns
		.filter((turn) => turn.speakerID === speaker.id)
		.map((turn) => turn.text.trim())
		.filter((text) => text.length > 0);
	const text =
		own.find((line) => line.split(/\s+/).length >= SNIPPET_MIN_WORDS) ??
		own.reduce(
			(longest, line) => (line.length > longest.length ? line : longest),
			"",
		);
	if (text.length <= SNIPPET_MAX_LENGTH) {
		return text;
	}
	const cut = text.slice(0, SNIPPET_MAX_LENGTH);
	const space = cut.lastIndexOf(" ");
	return `${(space > SNIPPET_MAX_LENGTH / 2 ? cut.slice(0, space) : cut).replace(/[\s,.;:!?-]+$/, "")}…`;
}

export interface SpeakerGroups {
	/** Confirmed speakers in cluster order, each with the others the same person owns. */
	named: { speaker: Speaker; also: Speaker[] }[];
	/** Not confirmed, and not suggested as a person who is already named here. */
	unnamed: Speaker[];
	/** How many speakers are confirmed, for "2 of 3 named". */
	namedCount: number;
}

/**
 * The popover's two sections. A speaker suggested or confirmed as a person
 * who already owns a confirmed speaker here is most likely that person heard
 * as two voices; it goes under the first one (`also`) rather than into
 * "Need names". Naming it the same person merges the two in the store.
 */
export function groupSpeakers(speakers: readonly Speaker[]): SpeakerGroups {
	const named: SpeakerGroups["named"] = [];
	const owners = new Map<string, Speaker[]>();
	for (const speaker of speakers) {
		if (!isNamed(speaker)) {
			continue;
		}
		const also = speaker.personID ? owners.get(speaker.personID) : undefined;
		if (also) {
			also.push(speaker);
			continue;
		}
		const entry = { speaker, also: [] as Speaker[] };
		named.push(entry);
		if (speaker.personID) {
			owners.set(speaker.personID, entry.also);
		}
	}
	const unnamed: Speaker[] = [];
	for (const speaker of speakers) {
		if (isNamed(speaker)) {
			continue;
		}
		const also = speaker.personID ? owners.get(speaker.personID) : undefined;
		if (also) {
			also.push(speaker);
		} else {
			unnamed.push(speaker);
		}
	}
	return {
		named,
		unnamed,
		namedCount: speakers.filter(isNamed).length,
	};
}

/** The speakers in the order the popover lists them, for moving to the next. */
export function listedOrder(groups: SpeakerGroups): Speaker[] {
	return [
		...groups.named.flatMap(({ speaker, also }) => [speaker, ...also]),
		...groups.unnamed,
	];
}

/**
 * The first speaker after `current` in the popover's order that still needs
 * a name, wrapping round; undefined once nobody else does.
 */
export function nextToName(
	speakers: readonly Speaker[],
	current: string,
): Speaker | undefined {
	const order = listedOrder(groupSpeakers(speakers));
	const at = order.findIndex((speaker) => speaker.id === current);
	const rotated = [...order.slice(at + 1), ...order.slice(0, Math.max(at, 0))];
	return rotated.find((speaker) => speaker.id !== current && !isNamed(speaker));
}

export type OptionGroupID = "meeting" | "people" | "create";

export interface OptionGroup {
	id: OptionGroupID;
	/** The heading; the create group has none on screen. */
	label: string;
	options: SpeakerOption[];
}

/**
 * The host's ranked options in the field's three groups, order kept within
 * each: everyone with a reason to be offered (the voice match, a name heard
 * in the meeting, an attendee, a person already speaking here; the host
 * puts that reason in `detail`), then the people without one (recent people
 * while the field is empty, every match once something is typed), then the
 * row that creates the typed name. The host's "unknown" row is not offered:
 * a speaker left unnamed stays in "Need names".
 */
export function groupOptions(
	options: readonly SpeakerOption[],
	query: string,
): OptionGroup[] {
	const groups: OptionGroup[] = [
		{ id: "meeting", label: "In this meeting", options: [] },
		{
			id: "people",
			label: query.trim() ? "People" : "Recent people",
			options: [],
		},
		{ id: "create", label: "New person", options: [] },
	];
	const [meeting, people, create] = groups as [
		OptionGroup,
		OptionGroup,
		OptionGroup,
	];
	for (const option of options) {
		if (option.kind === "unknown") {
			continue;
		}
		if (option.detail) {
			meeting.options.push(option);
		} else if (option.kind === "person") {
			people.options.push(option);
		} else {
			create.options.push(option);
		}
	}
	return groups.filter((group) => group.options.length > 0);
}

/** The text an option's row reads: a name, or `Create “name”`. */
export function optionText(option: SpeakerOption): string {
	return option.kind === "create" && !option.detail
		? `Create “${option.label}”`
		: option.label;
}
