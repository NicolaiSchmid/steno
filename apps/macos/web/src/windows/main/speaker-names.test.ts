import { describe, expect, it } from "vitest";
import type { SpeakerOption } from "@/bridge/contract";
import {
	groupOptions,
	nextToName,
	panelLayout,
	peopleLabel,
	pillState,
	placements,
	rowOrder,
	type Speaker,
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
	};
}

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
			expect(state.count).toBe(2);
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
			detail: "Sounds like",
		},
		{ kind: "person", label: "Lea", personID: person(4), detail: "Attendee" },
		{
			kind: "person",
			label: "Nicolai",
			personID: person(1),
			detail: "In this meeting",
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
			"Sounds like",
			"Invited",
		]);
		expect(groups[1]?.rows[0]?.combines).toBe("combines with Speaker 1");
		expect(
			groupOptions(options, "a", voice(2, "unknown"), speakers)[2]?.label,
		).toBe("People");
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
});
