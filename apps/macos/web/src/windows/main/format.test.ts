import { describe, expect, it } from "vitest";
import {
	createFormatter,
	firstSentence,
	formatPeople,
	formatRetention,
	formatSource,
} from "./format";

const f = createFormatter({ locale: "en-US", timeZone: "Europe/Berlin" });
// Wednesday 30 September 2026, 10:00 in Berlin.
const now = new Date("2026-09-30T08:00:00.000Z");

describe("durations", () => {
	it("writes minutes and seconds, hours when needed", () => {
		expect(f.duration(2738)).toBe("45:38");
		expect(f.duration(5)).toBe("00:05");
		expect(f.duration(3723)).toBe("1:02:03");
		expect(f.duration(-4)).toBe("00:00");
	});

	it("writes a turn's range", () => {
		expect(f.range(12, 41)).toBe("00:12 – 00:41");
	});

	it("words the time left", () => {
		expect(f.remaining(30)).toBe("less than a minute left");
		expect(f.remaining(60)).toBe("about a minute left");
		expect(f.remaining(95)).toBe("about 2 min left");
	});
});

describe("times and dates", () => {
	it("uses the 24-hour clock in the given zone", () => {
		expect(f.time("2026-09-29T12:50:00.000Z")).toBe("14:50");
	});

	it("writes the weekday and short date", () => {
		expect(f.date("2026-09-29T12:50:00.000Z")).toBe("Tuesday, Sep 29");
		expect(f.when("2026-09-29T12:50:00.000Z")).toBe("Tuesday, Sep 29 · 14:50");
	});

	it("adds the year only when it is not the current one", () => {
		expect(f.shortDate("2026-10-29T12:50:00.000Z", now)).toBe("Oct 29");
		expect(f.shortDate("2025-10-29T12:50:00.000Z", now)).toBe("Oct 29, 2025");
	});

	it("labels day groups relative to today", () => {
		expect(f.dayLabel("2026-09-30", now)).toEqual({
			label: "Today",
			date: "Wednesday, Sep 30",
		});
		expect(f.dayLabel("2026-09-29", now)).toEqual({
			label: "Yesterday",
			date: "Tuesday, Sep 29",
		});
		expect(f.dayLabel("2026-09-25", now)).toEqual({
			label: "Friday",
			date: "Sep 25",
		});
		expect(f.dayLabel("2025-12-24", now)).toEqual({
			label: "Wednesday",
			date: "Dec 24, 2025",
		});
	});

	it("words how long ago something happened", () => {
		expect(f.relative("2026-09-30T07:58:00.000Z", now)).toBe("2 min. ago");
		expect(f.relative("2026-09-30T05:00:00.000Z", now)).toBe("3 hr. ago");
		expect(f.relative("2026-09-29T08:00:00.000Z", now)).toBe("yesterday");
		expect(f.relative("2026-09-30T07:59:50.000Z", now)).toBe("now");
	});

	it("names the language", () => {
		expect(f.language("de")).toBe("German");
		expect(f.language("xx-unknown")).toBe("xx-unknown");
	});

	it("puts a due date on a weekday within the week", () => {
		expect(f.dueDate("2026-09-30T12:00:00.000Z", now)).toBe("Today");
		expect(f.dueDate("2026-10-01T12:00:00.000Z", now)).toBe("Tomorrow");
		expect(f.dueDate("2026-10-02T12:50:00.000Z", now)).toBe("Friday");
		expect(f.dueDate("2026-10-20T12:50:00.000Z", now)).toBe("Oct 20");
	});
});

describe("words", () => {
	it("names the source", () => {
		expect(formatSource("call")).toBe("Call");
		expect(formatSource("inPerson")).toBe("In person");
		expect(formatSource("phone")).toBe("iPhone");
	});

	it("words the retention", () => {
		expect(
			formatRetention(
				{
					kind: "deletesOn",
					deletesAt: "2026-10-29T12:50:00.000Z",
					keepsAudio: false,
					showsKeepToggle: true,
					filesExist: true,
				},
				f,
				now,
			),
		).toBe("Deletes Oct 29");
		expect(
			formatRetention(
				{
					kind: "keptForever",
					keepsAudio: true,
					showsKeepToggle: true,
					filesExist: true,
				},
				f,
				now,
			),
		).toBe("Recording kept");
	});

	it("takes the first sentence of a failure reason", () => {
		expect(firstSentence("Transcription failed: model not installed")).toBe(
			"Transcription failed: model not installed",
		);
		expect(firstSentence("Model missing. Install it in Settings.")).toBe(
			"Model missing.",
		);
		expect(firstSentence("\n\nv1.2 failed. Retry.")).toBe("v1.2 failed.");
		expect(firstSentence("   ")).toBe("No reason was reported.");
	});

	it("lists the people", () => {
		const speaker = (
			displayName: string,
			assignment: "confirmed" | "unknown",
		) => ({
			id: "00000000-0000-0000-0000-000000000001",
			clusterLabel: "Speaker",
			displayName,
			assignment,
			colorIndex: 0,
			hasClip: false,
			isPlaying: false,
		});
		expect(
			formatPeople([
				speaker("Nicolai", "confirmed"),
				speaker("Anna", "confirmed"),
				speaker("Speaker 3", "unknown"),
			]),
		).toBe("Nicolai, Anna +1");
		expect(formatPeople([speaker("Speaker 1", "unknown")])).toBe(
			"1 unnamed speaker",
		);
		expect(formatPeople([])).toBe("No speakers yet");
	});
});
