import type {
	MeetingDetailSnapshot,
	SpeakerOption,
	SpeakerOptionReason,
	VoiceStrength,
} from "@/bridge/contract";
import { personColor } from "@/lib/person-color";

export type Speaker = MeetingDetailSnapshot["speakers"][number];
export type Turn = MeetingDetailSnapshot["transcript"][number];
/** A voice combined into a speaker, which Separate again makes its own again. */
export type CombinedVoice = Speaker["combined"][number];
/** What plays a voice sample: a speaker, or a voice combined into one. */
export type Voice = Pick<
	Speaker,
	"id" | "clusterLabel" | "hasClip" | "isPlaying"
>;

/**
 * The cluster label of the microphone's speaker, the person at this
 * computer (`ME_SPEAKER_LABEL` in `steno-pipeline`). It has no sample clip:
 * the mic is the user's own voice.
 */
const ME_LABEL = "Me";

/** The progress stage while the pipeline finds the meeting's speakers. */
export const DIARIZE_STAGE = "diarize";

export function isMe(speaker: Pick<Speaker, "clusterLabel">): boolean {
	return speaker.clusterLabel === ME_LABEL;
}

export function isNamed(speaker: Speaker): boolean {
	return speaker.assignment === "confirmed";
}

export function isSuggested(speaker: Speaker): boolean {
	return speaker.assignment === "suggested";
}

/** Not a person (music, noise): out of the counts and the work. */
export function isDismissed(speaker: Speaker): boolean {
	return speaker.dismissed;
}

/** What a voice's own turns read once it is not a person. */
export const NOT_A_PERSON = "Not a person";

/** Whether the row may offer the sample clip: never for Me, else with a clip. */
export function canPlay(
	speaker: Pick<Speaker, "clusterLabel" | "hasClip">,
): boolean {
	return speaker.hasClip && !isMe(speaker);
}

export function firstName(name: string): string {
	return name.trim().split(/\s+/)[0] ?? name;
}

/** A voice match's strength as a word after its reason: " · strong". */
function strengthSuffix(strength: VoiceStrength | undefined): string {
	return strength ? ` · ${strength}` : "";
}

/**
 * Why a suggestion is offered, in the host's words, with how strongly the
 * voice matches: "Sounds like Anna · strong".
 */
export function suggestionReason(speaker: Speaker): string {
	return `Sounds like ${speaker.suggestionName ?? speaker.displayName}${strengthSuffix(speaker.suggestionStrength)}`;
}

/**
 * The voice back to unnamed, as the host republishes it after a No: no
 * person, no suggestion, its label for a name.
 */
export function unsuggested(speaker: Speaker): Speaker {
	const {
		suggestionName: _name,
		suggestionStrength: _strength,
		personID: _person,
		...rest
	} = speaker;
	return {
		...rest,
		assignment: "unknown",
		displayName: speaker.clusterLabel,
	};
}

/** What the transcript's name and the field's placeholder call a voice. */
export function voiceLabel(speaker: Speaker): string {
	return isMe(speaker) ? "You" : speaker.clusterLabel;
}

/**
 * What the voice says in its sample clip, so the quote is what plays (the
 * host's `quote`); empty when it says nothing.
 */
export function quote(speaker: Speaker): string {
	return speaker.quote?.trim() ?? "";
}

/**
 * The voice's nearby words when no exact quote exists (the host's
 * `context`: turns only partly inside the clip, or its longest turn when
 * it has no clip); empty when there is a quote or nothing. Never shown in
 * quotation marks: it is not what plays.
 */
export function context(speaker: Speaker): string {
	return quote(speaker) ? "" : (speaker.context?.trim() ?? "");
}

/* ---- A transcript turn's panel ------------------------------------------ */

/**
 * What a pick on a transcript turn changes: this paragraph only
 * (`transcript.setTurnSpeaker`), or the voice in the whole meeting
 * (`speakers.select`).
 */
export type PickScope = "turn" | "voice";

/**
 * The scope a turn's panel opens on (S2): all of the voice while it is
 * unnamed or only suggested, this paragraph once it is confirmed (Me
 * included). A turn without a voice has only this paragraph.
 */
export function defaultScope(speaker: Speaker | undefined): PickScope {
	return speaker && !isNamed(speaker) ? "voice" : "turn";
}

/**
 * The segmented choice's All: "All 4 paragraphs of Speaker 2", of the
 * confirmed person by name ("All 4 paragraphs of Nicolai"), "All of yours"
 * for Me, and "All of Speaker 2" for a voice with one paragraph.
 */
export function allScopeLabel(speaker: Speaker, paragraphs: number): string {
	if (isMe(speaker)) {
		return "All of yours";
	}
	const who = isNamed(speaker) ? speaker.displayName : speaker.clusterLabel;
	return paragraphs === 1
		? `All of ${who}`
		: `All ${paragraphs} paragraphs of ${who}`;
}

/** The scope line under This paragraph: what moves, and what undoes it. */
export const TURN_SCOPE_LINE =
	"Moves only this paragraph. Processing again resets this change.";

/** The red line when the host refused a move as `stale`. */
export const STALE_MOVE = "The transcript changed since you opened this.";

/**
 * The turn's segments as `transcript.setTurnSpeaker` takes them, each with
 * the voice the page shows it under (none for a turn without one).
 */
export function turnSegments(turn: Turn): { id: string; speakerID?: string }[] {
	return turn.segmentIDs.map((id) =>
		turn.speakerID ? { id, speakerID: turn.speakerID } : { id },
	);
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
 * or suggested, every voice not yet confirmed is amber work ("2 need
 * names": an unnamed voice and a suggestion both need a name), and when
 * only suggestions are left it asks to check them. All confirmed, it lists
 * the people. A voice that is not a person counts for nothing; when no
 * voice is a person the pill is done with nobody ("No speakers").
 */
export function pillState(
	all: readonly Speaker[],
	hasSpeech: boolean,
	finding: boolean,
): PillState {
	if (finding) {
		return { kind: "finding" };
	}
	if (all.length === 0 || !hasSpeech) {
		return { kind: "hidden" };
	}
	const speakers = all.filter((speaker) => !isDismissed(speaker));
	if (speakers.length === 0) {
		// Every voice is music or noise: nothing to name, nobody to list.
		return { kind: "done", avatars: [] };
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
	const suggested = speakers.filter(isSuggested).length;
	if (unknown.length > 0) {
		return { kind: "needs", count: unknown.length + suggested, avatars };
	}
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
 * (a voice match), "Need names", under a person in "Speakers", as that
 * person's lead row, another confirmed voice of theirs, or a voice that
 * sounds like them ("Also Nicolai?"), or at the bottom under "Not people".
 */
export type Placement =
	| { section: "me" | "check" | "need" | "dismissed" }
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
	/** Not people: dismissed voices, each with Restore. */
	dismissed: Speaker[];
}

/** Where each speaker belongs now, in cluster order. */
export function placements(
	speakers: readonly Speaker[],
): Map<string, Placement> {
	const owners = new Map<string, string>();
	const result = new Map<string, Placement>();
	for (const speaker of speakers) {
		if (isDismissed(speaker)) {
			result.set(speaker.id, { section: "dismissed" });
		}
	}
	for (const speaker of speakers) {
		if (!isNamed(speaker) || isDismissed(speaker)) {
			continue;
		}
		const personID = speaker.personID ?? speaker.id;
		const role = owners.has(personID) ? "voice" : "lead";
		owners.set(personID, owners.get(personID) ?? speaker.id);
		result.set(speaker.id, { section: "people", personID, role });
	}
	for (const speaker of speakers) {
		if (isNamed(speaker) || isDismissed(speaker)) {
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
 * panel opened, so a row confirmed, dismissed or restored during the visit
 * stays in its section (drawn as it is now) instead of jumping; a speaker
 * the host merged away is gone, and one the panel has not seen yet goes
 * where it belongs now.
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
		dismissed: [],
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

/** The voices combined into a person's voices, nested under the person. */
export function combinedOf(entry: PersonEntry): CombinedVoice[] {
	return [entry.speaker, ...entry.voices].flatMap((voice) => voice.combined);
}

/** A speaker's row and, nested under it, the voices combined into it. */
function andCombined(speaker: Speaker): (Speaker | CombinedVoice)[] {
	return [speaker, ...speaker.combined];
}

/**
 * The panel's rows top to bottom, for the keyboard and for advancing: the
 * speakers, each voice combined into one under it in every section (a
 * person's after their own voices), so Separate again stays reachable
 * after Remove name on the voice it joined.
 */
export function panelRows(layout: PanelLayout): (Speaker | CombinedVoice)[] {
	return [
		...(layout.me ? andCombined(layout.me) : []),
		...layout.check.flatMap(andCombined),
		...layout.need.flatMap(andCombined),
		...layout.people.flatMap((entry) => [
			entry.speaker,
			...entry.voices,
			...combinedOf(entry),
			...entry.also,
		]),
		...layout.dismissed.flatMap(andCombined),
	];
}

/** A speaker's row, not a voice combined into one. */
function isSpeakerRow(row: Speaker | CombinedVoice): row is Speaker {
	return "assignment" in row;
}

/** The first row with work: unnamed Me, a suggestion, an unnamed voice. */
export function firstWithWork(layout: PanelLayout): Speaker | undefined {
	return panelRows(layout)
		.filter(isSpeakerRow)
		.find((speaker) => !isNamed(speaker) && !isDismissed(speaker));
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
	const order = panelRows(layout);
	const at = order.findIndex((row) => row.id === current);
	return order
		.slice(at + 1)
		.filter(isSpeakerRow)
		.find(
			(speaker) =>
				speaker.assignment === "unknown" &&
				!isDismissed(speaker) &&
				!pending.has(speaker.id),
		);
}

/* ---- The field's options ------------------------------------------------- */

export interface OptionRow {
	option: SpeakerOption;
	/** The muted reason at the trailing edge: "Sounds like", "Invited". */
	reason?: string;
	/** For a person who owns another voice here: "combines with Speaker 2". */
	combines?: string;
	/** The avatar's colour, the voice's once picked (`optionColor`); none for Create. */
	colorIndex?: number;
}

export interface OptionGroup {
	id: "suggested" | "also" | "people" | "create";
	/** The heading; the create group has none on screen. */
	label: string;
	rows: OptionRow[];
}

/**
 * The host's reasons as the field shows them; a person who owns another
 * voice here is "Also speaking here" instead, by the snapshot.
 */
const REASONS: Partial<Record<SpeakerOptionReason, string>> = {
	voice: "Sounds like",
	mentioned: "Mentioned",
	invited: "Invited",
	lastInvitee: "Last invitee left",
};

/** An option's reason in words, a voice match with its strength. */
export function optionReason(option: SpeakerOption): string | undefined {
	const reason = option.reason ? REASONS[option.reason] : undefined;
	return reason && `${reason}${strengthSuffix(option.strength)}`;
}

/**
 * The colour a voice will have once `option` is confirmed: that of the
 * person's voice already in this meeting, else the person's own (the
 * colour rule of `personColor`). A Create has no person yet, so no colour.
 */
export function optionColor(
	option: SpeakerOption,
	speakers: readonly Speaker[],
): number | undefined {
	if (!option.personID) {
		return undefined;
	}
	return (
		speakers.find((other) => other.personID === option.personID)?.colorIndex ??
		personColor(option.personID)
	);
}

/**
 * A turn without a voice, nothing typed: the people named in this meeting,
 * once each, from the snapshot. The host's options are a voice's own (its
 * Noes left out, its evidence ranked first), and this paragraph has no
 * voice, so nothing here is asked of the host.
 */
export function meetingPeopleOptions(
	speakers: readonly Speaker[],
): SpeakerOption[] {
	const seen = new Set<string>();
	const options: SpeakerOption[] = [];
	for (const speaker of speakers) {
		const personID = speaker.personID;
		if (
			isNamed(speaker) &&
			!isDismissed(speaker) &&
			personID &&
			!seen.has(personID)
		) {
			seen.add(personID);
			options.push({
				kind: "person",
				label: speaker.displayName,
				personID,
			});
		}
	}
	return options;
}

/**
 * The host's ranked options in the field's groups, order kept within each:
 * Suggested (a voice match, a name heard in the meeting, an invitee; only
 * "Sounds like" is voice evidence), Also speaking here (people who already
 * own another confirmed voice of this meeting, from the snapshot's
 * `personID`, each saying which voice it would combine with), People
 * ("Recent" while nothing is typed; the person a confirmed voice already
 * is leads it as "Current"), and the row that creates the typed name.
 * `excluded` leaves out the person the user just said No to, while
 * nothing is typed: typed, they are offered again among the people, never
 * as a suggestion, as the hosts offer a person answered No for this voice
 * only when typed. For one paragraph (`scope` "turn") nothing combines:
 * the paragraph joins that person's voice, so no row says it combines.
 * `speaker` is none for a turn without a voice.
 */
export function groupOptions(
	options: readonly SpeakerOption[],
	query: string,
	speaker: Speaker | undefined,
	speakers: readonly Speaker[],
	excluded?: string,
	scope: PickScope = "voice",
): OptionGroup[] {
	const typed = query.trim() !== "";
	const groups: OptionGroup[] = [
		{ id: "suggested", label: "Suggested", rows: [] },
		{ id: "also", label: "Also speaking here", rows: [] },
		{ id: "people", label: typed ? "People" : "Recent", rows: [] },
		{ id: "create", label: "New person", rows: [] },
	];
	const [suggested, also, people, create] = groups as [
		OptionGroup,
		OptionGroup,
		OptionGroup,
		OptionGroup,
	];
	for (const option of options) {
		if (!typed && option.personID && option.personID === excluded) {
			continue;
		}
		const owner = option.personID
			? speakers.find(
					(other) =>
						other.id !== speaker?.id &&
						isNamed(other) &&
						other.personID === option.personID,
				)
			: undefined;
		// Said No to and typed again: a person to pick, no longer a suggestion.
		const answered =
			option.personID !== undefined && option.personID === excluded;
		const reason = answered ? undefined : optionReason(option);
		const color = optionColor(option, speakers);
		const colorIndex = color === undefined ? {} : { colorIndex: color };
		if (option.kind === "create") {
			// An invitee or a heard name Steno does not know yet is still a
			// suggestion, with its reason.
			(reason ? suggested : create).rows.push(
				reason ? { option, reason } : { option },
			);
		} else if (
			speaker &&
			isNamed(speaker) &&
			option.personID &&
			option.personID === speaker.personID
		) {
			// Who the voice is confirmed as already: no suggestion of itself.
			people.rows.unshift({ option, reason: "Current", ...colorIndex });
		} else if (owner) {
			also.rows.push(
				scope === "voice"
					? {
							option,
							combines: `combines with ${owner.clusterLabel}`,
							...colorIndex,
						}
					: { option, ...colorIndex },
			);
		} else if (reason) {
			suggested.rows.push({ option, reason, ...colorIndex });
		} else {
			people.rows.push({ option, ...colorIndex });
		}
	}
	return groups.filter((group) => group.rows.length > 0);
}

/** The text an option's row reads: a name, or `Create “name”`. */
export function optionText(option: SpeakerOption): string {
	return option.kind === "create" ? `Create “${option.label}”` : option.label;
}
