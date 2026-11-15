import { describe, expect, it } from "vitest";
import type { SpeakerOption } from "@/bridge/contract";
import { personColor } from "@/lib/person-color";
import {
	allScopeLabel,
	context,
	defaultScope,
	firstWithWork,
	groupOptions,
	nextToName,
	optionReason,
	panelLayout,
	peopleLabel,
	pillState,
	placements,
	quote,
	rowIDs,
	rowOrder,
	type Speaker,
	suggestionReason,
	type Turn,
	turnSegments,
	unsuggested,
} from "./speaker-names";

const id = (n: number) =>
	`00000000-0000-0000-0000-${String(n).padStart(12, "0")}`;
const person = (n: number) =>
	`00000000-0000-0000-0000-0000000000${String(n).padStart(2, "0")}`;

function voice(
	n: number,
	assignment: Speaker["assignment"],
	name?: string,
	personNumber?: number,
): Speaker {
	return {
		id: id(n),
		clusterLabel: `Speaker ${n}`,
		displayName: name ?? `Speaker ${n}`,
		assignment,
		...(personNumber ? { personID: person(personNumber) } : {}),
		colorIndex: n,
		hasClip: true,
		isPlaying: false,
		dismissed: false,
		combined: [],
	};
}

/** `speaker` dismissed as not a person. */
const notPerson = (speaker: Speaker): Speaker => ({
	...speaker,
	dismissed: true,
});

const ME: Speaker = { ...voice(9, "unknown", "Me"), clusterLabel: "Me" };

describe("pillState", () => {
	it("finds speakers, then hides without speech", () => {
		expect(pillState([], false, true)).toEqual({ kind: "finding" });
		expect(pillState([], true, false)).toEqual({ kind: "hidden" });
		expect(pillState([voice(1, "unknown")], false, false)).toEqual({
			kind: "hidden",
		});
	});

	it("is dashed only while nothing is named or suggested", () => {
		expect(
			pillState([voice(1, "unknown"), voice(2, "unknown")], true, false).kind,
		).toBe("none");
	});

	it("is amber with up to two avatars once anything is named or suggested", () => {
		const state = pillState(
			[
				voice(1, "confirmed", "Nicolai", 1),
				voice(2, "confirmed", "Jérôme", 2),
				voice(3, "suggested", "Anna", 3),
				voice(4, "unknown"),
				ME,
			],
			true,
			false,
		);
		expect(state.kind).toBe("needs");
		if (state.kind === "needs") {
			// Speaker 4 and Me need names, and Anna's suggestion a check.
			expect(state.count).toBe(3);
			expect(state.avatars.map((speaker) => speaker.displayName)).toEqual([
				"Nicolai",
				"Jérôme",
			]);
		}
	});

	it("asks to check when only suggestions are left, then lists the people", () => {
		expect(
			pillState(
				[voice(1, "confirmed", "Nicolai", 1), voice(2, "suggested", "Anna", 3)],
				true,
				false,
			),
		).toMatchObject({ kind: "check", count: 1 });
		const done = pillState(
			[
				voice(1, "confirmed", "Nicolai Schmid", 1),
				voice(2, "confirmed", "Nicolai Schmid", 1),
				voice(3, "confirmed", "Anna", 3),
			],
			true,
			false,
		);
		expect(done.kind).toBe("done");
		if (done.kind === "done") {
			expect(peopleLabel(done.avatars)).toBe("Nicolai, Anna");
		}
	});

	it("lists four first names, then +N", () => {
		const people = [1, 2, 3, 4, 5, 6].map((n) =>
			voice(n, "confirmed", `P${n} X`, n),
		);
		expect(peopleLabel(people)).toBe("P1, P2, P3, P4 +2");
	});
});

describe("panelLayout", () => {
	const speakers = [
		ME,
		voice(1, "confirmed", "Nicolai", 1),
		voice(2, "suggested", "Anna", 3),
		voice(3, "unknown"),
		voice(4, "suggested", "Nicolai", 1),
		voice(5, "confirmed", "Nicolai", 1),
		voice(6, "unknown"),
	];

	it("puts Me, the matches, the unnamed and the people in their sections", () => {
		const layout = panelLayout(speakers);
		expect(layout.me?.id).toBe(ME.id);
		expect(layout.check.map((speaker) => speaker.id)).toEqual([id(2)]);
		expect(layout.need.map((speaker) => speaker.id)).toEqual([id(3), id(6)]);
		expect(layout.people).toHaveLength(1);
		expect(layout.people[0]?.voices.map((speaker) => speaker.id)).toEqual([
			id(5),
		]);
		expect(layout.people[0]?.also.map((speaker) => speaker.id)).toEqual([
			id(4),
		]);
	});

	it("keeps rows where they were while the panel is open", () => {
		const frozen = placements(speakers);
		const after = speakers.map((speaker) =>
			speaker.id === id(3)
				? voice(3, "confirmed", "Jérôme", 2)
				: speaker.id === id(2)
					? voice(2, "confirmed", "Anna", 3)
					: speaker,
		);
		const layout = panelLayout(after, frozen);
		expect(layout.need.map((speaker) => speaker.displayName)).toEqual([
			"Jérôme",
			"Speaker 6",
		]);
		expect(layout.check.map((speaker) => speaker.displayName)).toEqual([
			"Anna",
		]);
		// Without the frozen places they would move to Speakers.
		expect(panelLayout(after).people.map(({ speaker }) => speaker.id)).toEqual([
			id(1),
			id(2),
			id(3),
		]);
	});

	it("drops a voice the host merged away", () => {
		const frozen = placements(speakers);
		const merged = speakers.filter((speaker) => speaker.id !== id(4));
		expect(panelLayout(merged, frozen).people[0]?.also).toEqual([]);
	});

	it("advances to the next unnamed voice, never a suggestion, never wrapping", () => {
		const layout = panelLayout(speakers);
		expect(rowOrder(layout)[0]?.id).toBe(ME.id);
		expect(nextToName(layout, ME.id)?.id).toBe(id(3));
		expect(nextToName(layout, id(3))?.id).toBe(id(6));
		expect(nextToName(layout, id(6))).toBeUndefined();
		expect(nextToName(layout, ME.id, new Set([id(3)]))?.id).toBe(id(6));
	});
});

describe("groupOptions", () => {
	const speakers = [voice(1, "confirmed", "Nicolai", 1), voice(2, "unknown")];
	const options: SpeakerOption[] = [
		{
			kind: "person",
			label: "Anna",
			personID: person(3),
			reason: "voice",
			strength: "strong",
		},
		{ kind: "person", label: "Lea", personID: person(4), reason: "invited" },
		{
			kind: "person",
			label: "Nicolai",
			personID: person(1),
			reason: "inThisMeeting",
		},
		{ kind: "person", label: "Paul", personID: person(5) },
		{ kind: "create", label: "Ann" },
	];

	it("groups Suggested, Also speaking here, People and Create", () => {
		const groups = groupOptions(options, "", voice(2, "unknown"), speakers);
		expect(groups.map((group) => group.label)).toEqual([
			"Suggested",
			"Also speaking here",
			"Recent",
			"New person",
		]);
		expect(groups[0]?.rows.map((row) => row.reason)).toEqual([
			"Sounds like · strong",
			"Invited",
		]);
		expect(groups[1]?.rows[0]?.combines).toBe("combines with Speaker 1");
		expect(
			groupOptions(options, "a", voice(2, "unknown"), speakers)[2]?.label,
		).toBe("People");
	});

	it("keeps a suggested name Steno does not know yet with the suggestions", () => {
		const groups = groupOptions(
			[{ kind: "create", label: "Maya", reason: "lastInvitee" }],
			"",
			voice(2, "unknown"),
			speakers,
		);
		expect(groups.map((group) => group.id)).toEqual(["suggested"]);
		expect(groups[0]?.rows[0]?.reason).toBe("Last invitee left");
	});

	it("shows each person in the colour the voice will take once picked", () => {
		const groups = groupOptions(options, "", voice(2, "unknown"), speakers);
		const colours = Object.fromEntries(
			groups
				.flatMap((group) => group.rows)
				.map((row) => [row.option.label, row.colorIndex]),
		);
		// Nicolai's voice here keeps its colour; the others take their own.
		expect(colours).toEqual({
			Anna: personColor(person(3)),
			Lea: personColor(person(4)),
			Nicolai: 1,
			Paul: personColor(person(5)),
			// A Create has no person yet, so no colour (a plus, not an avatar).
			Ann: undefined,
		});
	});

	it("shows the person a confirmed voice already is as Current, not as a suggestion", () => {
		const anna = voice(2, "confirmed", "Anna", 3);
		const groups = groupOptions(options, "", anna, [
			speakers[0] as Speaker,
			anna,
		]);
		expect(groups.find((group) => group.id === "suggested")?.rows).toEqual([
			expect.objectContaining({ reason: "Invited" }),
		]);
		expect(groups.find((group) => group.id === "people")?.rows[0]).toEqual(
			expect.objectContaining({
				option: expect.objectContaining({ label: "Anna" }),
				reason: "Current",
			}),
		);
	});

	it("leaves out the excluded person", () => {
		const groups = groupOptions(
			options,
			"",
			voice(2, "unknown"),
			speakers,
			person(1),
		);
		expect(groups.some((group) => group.id === "also")).toBe(false);
	});

	it("offers the excluded person again once their name is typed", () => {
		const groups = groupOptions(
			options,
			"Nic",
			voice(2, "unknown"),
			speakers,
			person(1),
		);
		expect(
			groups.find((group) => group.id === "also")?.rows[0]?.option.label,
		).toBe("Nicolai");
		// Said No to as a voice match: typed, a person, not a suggestion.
		const anna = groupOptions(
			options,
			"Ann",
			voice(2, "unknown"),
			speakers,
			person(3),
		);
		expect(anna.find((group) => group.id === "suggested")?.rows).toEqual([
			expect.objectContaining({ reason: "Invited" }),
		]);
		expect(
			anna
				.find((group) => group.id === "people")
				?.rows.map((row) => row.option.label),
		).toContain("Anna");
		// Whitespace is not typing: still left out.
		expect(
			groupOptions(options, "  ", voice(2, "unknown"), speakers, person(1))
				.flatMap((group) => group.rows)
				.some((row) => row.option.personID === person(1)),
		).toBe(false);
	});
});

describe("personColor", () => {
	it("folds a person's ID as both hosts do", () => {
		// The same ID and index as `colours_are_stable_fnv_folds` in steno-host.
		expect(personColor("00000000-0000-0000-0000-00000000000C")).toBe(1);
		expect(personColor("00000000-0000-0000-0000-00000000000c")).toBe(1);
	});
});

describe("voices that are not people", () => {
	const speakers = [
		voice(1, "confirmed", "Nicolai", 1),
		notPerson(voice(2, "unknown")),
		voice(3, "unknown"),
	];

	it("count for nothing in the pill", () => {
		expect(pillState(speakers, true, false)).toMatchObject({
			kind: "needs",
			count: 1,
		});
		// No voice is a person: done, with nobody to list ("No speakers").
		expect(pillState([notPerson(voice(2, "unknown"))], true, false)).toEqual({
			kind: "done",
			avatars: [],
		});
	});

	it("sit last, under Not people, and are no work", () => {
		const layout = panelLayout(speakers);
		expect(layout.dismissed.map((speaker) => speaker.id)).toEqual([id(2)]);
		expect(layout.need.map((speaker) => speaker.id)).toEqual([id(3)]);
		expect(rowOrder(layout).at(-1)?.id).toBe(id(2));
		expect(firstWithWork(panelLayout([notPerson(voice(2, "unknown"))]))).toBe(
			undefined,
		);
		// Past Nicolai only the voice that is not a person is left: no one.
		expect(nextToName(layout, id(1))).toBeUndefined();
	});

	it("keep the place they had when the panel opened", () => {
		const frozen = placements([voice(2, "unknown"), voice(3, "unknown")]);
		const layout = panelLayout(speakers, frozen);
		expect(layout.need.map((speaker) => speaker.id)).toEqual([id(2), id(3)]);
		expect(layout.dismissed).toEqual([]);
	});
});

describe("the evidence and the reasons", () => {
	it("quotes the clip's own words", () => {
		expect(quote({ ...voice(1, "unknown"), quote: " Hallo zusammen. " })).toBe(
			"Hallo zusammen.",
		);
		expect(quote(voice(1, "unknown"))).toBe("");
	});

	it("gives the nearby words only when there is no exact quote", () => {
		const near = { ...voice(1, "unknown"), context: " Also ich denke " };
		expect(context(near)).toBe("Also ich denke");
		expect(quote(near)).toBe("");
		expect(context({ ...near, quote: "Hallo zusammen." })).toBe("");
		expect(context(voice(1, "unknown"))).toBe("");
	});

	it("says how strongly a voice matches, in words", () => {
		const anna = {
			...voice(3, "suggested", "Anna", 3),
			suggestionName: "Anna",
		};
		expect(suggestionReason(anna)).toBe("Sounds like Anna");
		expect(suggestionReason({ ...anna, suggestionStrength: "likely" })).toBe(
			"Sounds like Anna · likely",
		);
		expect(
			optionReason({
				kind: "person",
				label: "Anna",
				reason: "voice",
				strength: "strong",
			}),
		).toBe("Sounds like · strong");
		expect(
			optionReason({ kind: "create", label: "Maya", reason: "lastInvitee" }),
		).toBe("Last invitee left");
		expect(
			optionReason({
				kind: "person",
				label: "Nicolai",
				reason: "inThisMeeting",
			}),
		).toBeUndefined();
	});

	it("drops a suggestion answered No as the host republishes it", () => {
		const anna = {
			...voice(3, "suggested", "Anna", 3),
			suggestionName: "Anna",
			suggestionStrength: "strong" as const,
		};
		expect(unsuggested(anna)).toEqual({
			...voice(3, "unknown"),
		});
	});
});

describe("a paragraph's scope", () => {
	it("opens on All while the voice is unnamed or suggested, on This paragraph once confirmed", () => {
		expect(defaultScope(voice(1, "unknown"))).toBe("voice");
		expect(defaultScope(voice(2, "suggested", "Anna", 3))).toBe("voice");
		expect(defaultScope(voice(3, "confirmed", "Jérôme", 4))).toBe("turn");
		expect(
			defaultScope({
				...voice(9, "confirmed", "Nicolai", 1),
				clusterLabel: "Me",
			}),
		).toBe("turn");
		expect(defaultScope(undefined)).toBe("turn");
	});

	it("names All by the voice, the person once confirmed, yours for Me", () => {
		expect(allScopeLabel(voice(2, "unknown"), 4)).toBe(
			"All 4 paragraphs of Speaker 2",
		);
		expect(allScopeLabel(voice(2, "suggested", "Anna", 3), 4)).toBe(
			"All 4 paragraphs of Speaker 2",
		);
		expect(allScopeLabel(voice(2, "confirmed", "Nicolai", 1), 4)).toBe(
			"All 4 paragraphs of Nicolai",
		);
		expect(allScopeLabel(voice(2, "unknown"), 1)).toBe("All of Speaker 2");
		expect(allScopeLabel(ME, 4)).toBe("All of yours");
	});

	it("sends every segment of the turn with the voice it shows, or none", () => {
		const turn: Turn = {
			id: id(100),
			speakerID: id(1),
			speakerName: "Speaker 1",
			startSeconds: 0,
			endSeconds: 4,
			text: "Hallo.",
			segmentIDs: [id(100), id(101)],
			isPlaying: false,
		};
		expect(turnSegments(turn)).toEqual([
			{ id: id(100), speakerID: id(1) },
			{ id: id(101), speakerID: id(1) },
		]);
		const { speakerID: _speaker, ...voiceless } = turn;
		expect(turnSegments(voiceless)).toEqual([{ id: id(100) }, { id: id(101) }]);
	});

	it("says nothing combines when one paragraph moves", () => {
		const nicolai = voice(1, "confirmed", "Nicolai", 1);
		const options: SpeakerOption[] = [
			{ kind: "person", label: "Nicolai", personID: person(1) },
		];
		const [also] = groupOptions(
			options,
			"",
			voice(2, "unknown"),
			[nicolai, voice(2, "unknown")],
			undefined,
			"turn",
		);
		expect(also?.id).toBe("also");
		expect(also?.rows[0]?.combines).toBeUndefined();
	});
});

describe("combined voices", () => {
	it("follow their person's voices in the keyboard's order", () => {
		const nicolai = {
			...voice(1, "confirmed", "Nicolai", 1),
			combined: [
				{
					id: id(5),
					clusterLabel: "Speaker 5",
					hasClip: true,
					isPlaying: false,
				},
			],
		};
		const second = voice(2, "confirmed", "Nicolai", 1);
		const unnamed = voice(3, "unknown");
		const layout = panelLayout([nicolai, second, unnamed]);
		expect(rowIDs(layout)).toEqual([id(3), id(1), id(2), id(5)]);
		expect(rowOrder(layout).map((speaker) => speaker.id)).toEqual([
			id(3),
			id(1),
			id(2),
		]);
	});
});
