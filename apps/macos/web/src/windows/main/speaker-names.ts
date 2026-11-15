import type { MeetingDetailSnapshot, SpeakerOption } from "@/bridge/contract";

export type Speaker = MeetingDetailSnapshot["speakers"][number];
export type Turn = MeetingDetailSnapshot["transcript"][number];

/**
 * The cluster label of the microphone's speaker, the person at this
 * computer (`ME_SPEAKER_LABEL` in `steno-pipeline`). It has no sample clip:
 * the mic is the user's own voice.
 */
const ME_LABEL = "Me";

/** The progress stage while the pipeline finds the meeting's speakers. */
export const DIARIZE_STAGE = "diarize";

export function isMe(speaker: Speaker): boolean {
	return speaker.clusterLabel === ME_LABEL;
}

export function isNamed(speaker: Speaker): boolean {
	return speaker.assignment === "confirmed";
}

export function isSuggested(speaker: Speaker): boolean {
	return speaker.assignment === "suggested";
}

/** Whether the row may offer the sample clip: never for Me, else with a clip. */
export function canPlay(speaker: Speaker): boolean {
	return speaker.hasClip && !isMe(speaker);
}

export function firstName(name: string): string {
	return name.trim().split(/\s+/)[0] ?? name;
}

/** Why a suggestion is offered, in the host's words: "Sounds like Anna". */
export function suggestionReason(speaker: Speaker): string {
	return `Sounds like ${speaker.suggestionName ?? speaker.displayName}`;
}

/** What the transcript's name and the field's placeholder call a voice. */
export function voiceLabel(speaker: Speaker): string {
	return isMe(speaker) ? "You" : speaker.clusterLabel;
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

/* ---- The header pill --------------------------------------------------- */

export type PillState =
	| { kind: "hidden" }
	| { kind: "finding" }
	| { kind: "none" }
	| { kind: "needs"; count: number; avatars: Speaker[] }
	| { kind: "check"; count: number; avatars: Speaker[] }
	| { kind: "done"; avatars: Speaker[] };

/** One speaker per person (the first voice), then the unnamed voices. */
function distinctPeople(speakers: readonly Speaker[]): Speaker[] {
	const seen = new Set<string>();
	return speakers.filter((speaker) => {
		const key = speaker.personID ?? speaker.id;
		if (seen.has(key)) {
			return false;
		}
		seen.add(key);
		return true;
	});
}

/**
 * What the header pill says. While the pipeline finds the meeting's
 * speakers it says so; without speech there is no pill. Nobody named and
 * nothing suggested is the dashed "Name speakers"; once anything is named
 * or suggested, unnamed voices are amber work ("2 need names"), and when
 * only suggestions are left it asks to check them. All confirmed, it lists
 * the people.
 */
export function pillState(
	speakers: readonly Speaker[],
	hasSpeech: boolean,
	finding: boolean,
): PillState {
	if (finding) {
		return { kind: "finding" };
	}
	if (speakers.length === 0 || !hasSpeech) {
		return { kind: "hidden" };
	}
	const unknown = speakers.filter(
		(speaker) => speaker.assignment === "unknown",
	);
	const known = distinctPeople(
		speakers.filter((speaker) => speaker.assignment !== "unknown"),
	);
	if (known.length === 0) {
		return { kind: "none" };
	}
	const avatars = known.slice(0, 2);
	if (unknown.length > 0) {
		return { kind: "needs", count: unknown.length, avatars };
	}
	const suggested = speakers.filter(isSuggested).length;
	if (suggested > 0) {
		return { kind: "check", count: suggested, avatars };
	}
	return { kind: "done", avatars: known };
}

/** "1 needs a name", "2 need names". */
export function needsLabel(count: number): string {
	return count === 1 ? "1 needs a name" : `${count} need names`;
}

/** "Check 1 name", "Check 2 names". */
export function checkLabel(count: number): string {
	return count === 1 ? "Check 1 name" : `Check ${count} names`;
}

/** The confirmed people's first names, four at most, then "+N". */
export function peopleLabel(people: readonly Speaker[]): string {
	const names = people
		.slice(0, 4)
		.map((speaker) => firstName(speaker.displayName));
	const more = people.length - names.length;
	return more > 0 ? `${names.join(", ")} +${more}` : names.join(", ");
}

/* ---- The panel ----------------------------------------------------------- */

/**
 * Where a speaker sits in the panel: the unnamed-Me line, "Is this right?"
 * (a voice match), "Need names", or under a person in "Speakers", as that
 * person's lead row, another confirmed voice of theirs, or a voice that
 * sounds like them ("Also Nicolai?").
 */
export type Placement =
	| { section: "me" | "check" | "need" }
	| { section: "people"; personID: string; role: "lead" | "voice" | "also" };

export interface PersonEntry {
	speaker: Speaker;
	/** The person's other confirmed voices, nested under the row. */
	voices: Speaker[];
	/** Voices suggested as this person ("Also Nicolai?"). */
	also: Speaker[];
}

export interface PanelLayout {
	me: Speaker | undefined;
	check: Speaker[];
	need: Speaker[];
	people: PersonEntry[];
}

/** Where each speaker belongs now, in cluster order. */
export function placements(
	speakers: readonly Speaker[],
): Map<string, Placement> {
	const owners = new Map<string, string>();
	const result = new Map<string, Placement>();
	for (const speaker of speakers) {
		if (!isNamed(speaker)) {
			continue;
		}
		const personID = speaker.personID ?? speaker.id;
		const role = owners.has(personID) ? "voice" : "lead";
		owners.set(personID, owners.get(personID) ?? speaker.id);
		result.set(speaker.id, { section: "people", personID, role });
	}
	for (const speaker of speakers) {
		if (isNamed(speaker)) {
			continue;
		}
		if (speaker.personID && owners.has(speaker.personID)) {
			result.set(speaker.id, {
				section: "people",
				personID: speaker.personID,
				role: "also",
			});
		} else if (isSuggested(speaker)) {
			result.set(speaker.id, { section: "check" });
		} else if (isMe(speaker)) {
			result.set(speaker.id, { section: "me" });
		} else {
			result.set(speaker.id, { section: "need" });
		}
	}
	return result;
}

/**
 * The panel's sections. `frozen` holds where each speaker sat when the
 * panel opened, so a row confirmed during the visit stays in its section
 * (marked done) instead of jumping; a speaker the host merged away is
 * gone, and one the panel has not seen yet goes where it belongs now.
 */
export function panelLayout(
	speakers: readonly Speaker[],
	frozen?: ReadonlyMap<string, Placement>,
): PanelLayout {
	const now = placements(speakers);
	const layout: PanelLayout = {
		me: undefined,
		check: [],
		need: [],
		people: [],
	};
	const people = new Map<string, PersonEntry>();
	const place = (speaker: Speaker) =>
		frozen?.get(speaker.id) ?? now.get(speaker.id);
	for (const speaker of speakers) {
		const where = place(speaker);
		if (where?.section === "people" && where.role === "lead") {
			const entry = { speaker, voices: [], also: [] };
			people.set(where.personID, entry);
			layout.people.push(entry);
		}
	}
	for (const speaker of speakers) {
		const where = place(speaker);
		if (!where) {
			continue;
		}
		if (where.section === "people") {
			if (where.role === "lead") {
				continue;
			}
			const entry = people.get(where.personID);
			if (entry) {
				entry[where.role === "voice" ? "voices" : "also"].push(speaker);
			} else {
				layout.need.push(speaker);
			}
		} else if (where.section === "me") {
			layout.me = speaker;
		} else {
			layout[where.section].push(speaker);
		}
	}
	return layout;
}

/** The panel's rows top to bottom, for the keyboard and for advancing. */
export function rowOrder(layout: PanelLayout): Speaker[] {
	return [
		...(layout.me ? [layout.me] : []),
		...layout.check,
		...layout.need,
		...layout.people.flatMap(({ speaker, voices, also }) => [
			speaker,
			...voices,
			...also,
		]),
	];
}

/** The first row with work: unnamed Me, a suggestion, an unnamed voice. */
export function firstWithWork(layout: PanelLayout): Speaker | undefined {
	return rowOrder(layout).find((speaker) => !isNamed(speaker));
}

/**
 * The next unnamed voice after `current` in the panel's order, for naming
 * to move on to once the host took a pick: never a suggestion, never one
 * already named or `pending`, and never wrapping back to the top.
 */
export function nextToName(
	layout: PanelLayout,
	current: string,
	pending: ReadonlySet<string> = new Set(),
): Speaker | undefined {
	const order = rowOrder(layout);
	const at = order.findIndex((speaker) => speaker.id === current);
	return order
		.slice(at + 1)
		.find(
			(speaker) => speaker.assignment === "unknown" && !pending.has(speaker.id),
		);
}

/* ---- The field's options ------------------------------------------------- */

export interface OptionRow {
	option: SpeakerOption;
	/** The muted reason at the trailing edge: "Sounds like", "Invited". */
	reason?: string;
	/** For a person who owns another voice here: "combines with Speaker 2". */
	combines?: string;
	/** The avatar's colour: the person's in this meeting, else from the name. */
	colorIndex: number;
}

export interface OptionGroup {
	id: "suggested" | "also" | "people" | "create";
	/** The heading; the create group has none on screen. */
	label: string;
	rows: OptionRow[];
}

/** The host's option tags as the field shows them. */
const REASONS: Record<string, string> = {
	"Sounds like": "Sounds like",
	Mentioned: "Mentioned",
	Attendee: "Invited",
};

function nameColor(name: string): number {
	let sum = 0;
	for (const char of name) {
		sum += char.codePointAt(0) ?? 0;
	}
	return sum % 4;
}

/**
 * The host's ranked options in the field's groups, order kept within each:
 * Suggested (a voice match, a name heard in the meeting, an invitee; only
 * "Sounds like" is voice evidence), Also speaking here (people who already
 * own another confirmed voice of this meeting, from the snapshot's
 * `personID`, each saying which voice it would combine with), People
 * ("Recent" while nothing is typed), and the row that creates the typed
 * name. `excluded` leaves a person out ("Someone else").
 */
export function groupOptions(
	options: readonly SpeakerOption[],
	query: string,
	speaker: Speaker,
	speakers: readonly Speaker[],
	excluded?: string,
): OptionGroup[] {
	const groups: OptionGroup[] = [
		{ id: "suggested", label: "Suggested", rows: [] },
		{ id: "also", label: "Also speaking here", rows: [] },
		{ id: "people", label: query.trim() ? "People" : "Recent", rows: [] },
		{ id: "create", label: "New person", rows: [] },
	];
	const [suggested, also, people, create] = groups as [
		OptionGroup,
		OptionGroup,
		OptionGroup,
		OptionGroup,
	];
	for (const option of options) {
		if (option.personID && option.personID === excluded) {
			continue;
		}
		const owner = option.personID
			? speakers.find(
					(other) =>
						other.id !== speaker.id &&
						isNamed(other) &&
						other.personID === option.personID,
				)
			: undefined;
		const reason = option.detail ? REASONS[option.detail] : undefined;
		const colorIndex =
			owner?.colorIndex ??
			speakers.find((other) => other.personID === option.personID)
				?.colorIndex ??
			nameColor(option.label);
		if (option.kind === "create") {
			create.rows.push({ option, colorIndex });
		} else if (owner) {
			also.rows.push({
				option,
				combines: `combines with ${owner.clusterLabel}`,
				colorIndex,
			});
		} else if (reason) {
			suggested.rows.push({ option, reason, colorIndex });
		} else {
			people.rows.push({ option, colorIndex });
		}
	}
	return groups.filter((group) => group.rows.length > 0);
}

/** The text an option's row reads: a name, or `Create “name”`. */
export function optionText(option: SpeakerOption): string {
	return option.kind === "create" ? `Create “${option.label}”` : option.label;
}
