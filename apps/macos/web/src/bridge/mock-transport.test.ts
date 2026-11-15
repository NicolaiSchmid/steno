import { describe, expect, it, vi } from "vitest";
import type { MeetingDetailSnapshot, ProgressSnapshot } from "./contract";
import {
	answerPlaying,
	answerSpeakerChange,
	answerSpeakerOptions,
	answerSpeakerSelect,
	answerSpeakerSeparate,
	answerTurnSpeaker,
	applyScenario,
	combineRecord,
	createMockTransport,
	fixtureKey,
	isSnapshotKey,
	loadFixtureReplies,
	loadFixtureSnapshots,
	queryFromLocation,
	replyMethod,
	replyMethods,
} from "./mock-transport";
import { BridgeError } from "./transport";

describe("fixture replies", () => {
	it("answers a call that arrives while the replies are still loading", async () => {
		const transport = createMockTransport();
		const [first, second] = await Promise.all([
			transport.call("page.ready", undefined),
			transport.call("speakers.options", { speakerID: "x", query: "" }),
		]);
		expect(first).toBeUndefined();
		expect(second).toMatchObject({ prefill: "Anna" });
	});
});

describe("scenarios", () => {
	it("aliases the generic reply shapes onto the native alerts and panels", async () => {
		expect(replyMethods("meetings.list")).toEqual([]);
		const replies = await loadFixtureReplies();
		for (const method of replyMethods("reply.confirm")) {
			expect(replies[method], method).toEqual({ confirmed: true });
		}
		expect(replies["speakers.options"]).toBeDefined();
	});

	it("reads the query from the search and the hash", () => {
		const params = queryFromLocation({
			search: "?scenario=empty",
			hash: "#/main?tab=notes&scenario=failed",
		});
		expect(params.get("scenario")).toBe("failed");
		expect(params.get("tab")).toBe("notes");
		expect(queryFromLocation(undefined).size).toBe(0);
	});

	it("passes the fixtures through without a scenario", async () => {
		const snapshots = await loadFixtureSnapshots();
		const result = applyScenario(snapshots, new URLSearchParams(""));
		expect(result["meetings.list"]).toBe(snapshots["meetings.list"]);
		expect(result["recording.live"]).toBeUndefined();
	});
});

describe("speaker naming outside the app", () => {
	const SPEAKER_4 = "00000000-0000-0000-0000-000000000017";
	const NICOLAI = "00000000-0000-0000-0000-00000000000A";
	const detail = async (scenario: string) =>
		applyScenario(
			await loadFixtureSnapshots(),
			new URLSearchParams(`scenario=${scenario}`),
		)["meeting.detail"] as MeetingDetailSnapshot;

	it("names a voice and every turn of it", async () => {
		const before = await detail("speakers-partly");
		const after = await answerSpeakerSelect(before, {
			speakerID: SPEAKER_4,
			option: { kind: "create", label: "Lea" },
			combine: false,
		});
		expect(after.speakers.find((row) => row.id === SPEAKER_4)).toMatchObject({
			assignment: "confirmed",
			displayName: "Lea",
		});
		expect(
			after.transcript
				.filter((turn) => turn.speakerID === SPEAKER_4)
				.map((turn) => turn.speakerName),
		).toEqual(["Lea"]);
	});

	it("refuses to combine without being asked, as the hosts do, then folds the voice in", async () => {
		const before = await detail("speakers-split");
		const pick = {
			speakerID: SPEAKER_4,
			option: { kind: "person" as const, label: "Nicolai", personID: NICOLAI },
		};
		expect(() =>
			answerSpeakerSelect(before, { ...pick, combine: false }),
		).toThrow(
			expect.objectContaining({
				code: "wouldCombine",
				message: "Nicolai already speaks as Speaker 1 in this meeting.",
			}),
		);
		const after = await answerSpeakerSelect(before, { ...pick, combine: true });
		expect(after.speakers.map((row) => row.id)).not.toContain(SPEAKER_4);
		expect(after.transcript.some((turn) => turn.speakerID === SPEAKER_4)).toBe(
			false,
		);
		// It nests under Nicolai's voice, and Separate again brings it back.
		const owner = after.speakers.find((row) => row.personID === NICOLAI);
		expect(owner?.combined.map((voice) => voice.id)).toContain(SPEAKER_4);
		const record = combineRecord(before, pick);
		const separated = answerSpeakerSeparate(
			after,
			{ speakerID: SPEAKER_4 },
			"",
			record,
		);
		expect(separated.speakers.find((row) => row.id === SPEAKER_4)).toEqual(
			before.speakers.find((row) => row.id === SPEAKER_4),
		);
		expect(separated.transcript).toEqual(before.transcript);
		expect(() =>
			answerSpeakerSeparate(separated, { speakerID: SPEAKER_4 }),
		).toThrow(expect.objectContaining({ code: "stale" }));
	});

	it("moves one paragraph to a new voice, refusing a stale one, as the hosts do", async () => {
		const before = await detail("speakers-partly");
		const turn = before.transcript.find(
			(entry) => entry.speakerID === SPEAKER_4,
		);
		if (!turn) throw new Error("no turn of Speaker 4");
		const params = {
			meetingID: before.id,
			generation: before.transcriptGeneration,
			segments: turn.segmentIDs.map((id) => ({ id, speakerID: SPEAKER_4 })),
			option: { kind: "create" as const, label: "Lea" },
		};
		const after = answerTurnSpeaker(before, params);
		const moved = after.transcript.find((entry) => entry.id === turn.id);
		const lea = after.speakers.find((row) => row.id === moved?.speakerID);
		expect(lea).toMatchObject({
			// Speaker 5 is combined into Nicolai's voice: the next is 6.
			clusterLabel: "Speaker 6",
			displayName: "Lea",
			assignment: "confirmed",
			hasClip: false,
		});
		// Speaker 4 led only that paragraph: it goes.
		expect(after.speakers.map((row) => row.id)).not.toContain(SPEAKER_4);
		expect(() => answerTurnSpeaker(after, params)).toThrow(
			expect.objectContaining({ code: "stale" }),
		);
		expect(() =>
			answerTurnSpeaker(before, {
				...params,
				generation: params.generation + 1,
			}),
		).toThrow(expect.objectContaining({ code: "stale" }));
		expect(() =>
			answerTurnSpeaker(before, params, "speakers-move-stale"),
		).toThrow(expect.objectContaining({ code: "stale" }));
	});

	it("marks the one sound playing", async () => {
		const before = await detail("speakers-partly");
		const [first] = before.transcript;
		if (!first) throw new Error("no turn");
		const playing = answerPlaying(before, { turn: first.id });
		expect(playing.transcript.map((turn) => turn.isPlaying)).toEqual(
			before.transcript.map((turn) => turn.id === first.id),
		);
		const voice = answerPlaying(playing, { voice: SPEAKER_4 });
		expect(voice.transcript.some((turn) => turn.isPlaying)).toBe(false);
		expect(voice.speakers.find((row) => row.id === SPEAKER_4)?.isPlaying).toBe(
			true,
		);
		expect(
			answerPlaying(voice, null).speakers.some((row) => row.isPlaying),
		).toBe(false);
	});

	it("keeps every speaker state valid and every turn's segments its own", async () => {
		const { meetingDetailSnapshot } = await import("./contract");
		for (const scenario of [
			"speakers-none",
			"speakers-me",
			"speakers-me-unnamed",
			"speakers-saving",
			"speakers-partly",
			"speakers-no-voice",
			"speakers-no-recording",
			"speakers-move-stale",
		]) {
			const snapshot = await detail(scenario);
			expect(meetingDetailSnapshot.safeParse(snapshot).success, scenario).toBe(
				true,
			);
			for (const turn of snapshot.transcript) {
				expect(turn.segmentIDs[0], scenario).toBe(turn.id);
			}
		}
	});

	it("answers No, Not a person, Restore and Remove name as the hosts do", async () => {
		const ANNA = "00000000-0000-0000-0000-000000000016";
		const ANNA_PERSON = "00000000-0000-0000-0000-00000000000C";
		const before = await detail("speakers-partly");
		const rejected = answerSpeakerChange(before, "speakers.reject", {
			speakerID: ANNA,
			personID: ANNA_PERSON,
		});
		expect(rejected.speakers.find((row) => row.id === ANNA)).toMatchObject({
			assignment: "unknown",
			displayName: "Speaker 3",
		});
		expect(
			rejected.speakers.find((row) => row.id === ANNA)?.suggestionName,
		).toBeUndefined();

		const dismissed = answerSpeakerChange(before, "speakers.dismiss", {
			speakerID: SPEAKER_4,
		});
		expect(
			dismissed.speakers.find((row) => row.id === SPEAKER_4)?.dismissed,
		).toBe(true);
		expect(
			dismissed.transcript
				.filter((turn) => turn.speakerID === SPEAKER_4)
				.map((turn) => turn.speakerName),
		).toEqual(["Not a person"]);
		const restored = answerSpeakerChange(dismissed, "speakers.restore", {
			speakerID: SPEAKER_4,
		});
		expect(restored.speakers.find((row) => row.id === SPEAKER_4)).toMatchObject(
			{
				dismissed: false,
				assignment: "unknown",
			},
		);

		const cleared = answerSpeakerChange(before, "speakers.clear", {
			speakerID: "00000000-0000-0000-0000-000000000014",
		});
		expect(cleared.speakers[0]).toMatchObject({
			assignment: "unknown",
			displayName: "Speaker 1",
		});
	});

	it("keeps a person answered No to out of that voice's options until typed", () => {
		const recorded = {
			options: [
				{
					kind: "person" as const,
					label: "Anna",
					personID: "p-anna",
					reason: "voice" as const,
					strength: "strong" as const,
				},
				{ kind: "person" as const, label: "Lea", personID: "p-lea" },
			],
		};
		const rejected = new Set(["s-3 p-anna"]);
		const labels = (query: string, speakerID = "s-3") =>
			(
				answerSpeakerOptions(recorded, { speakerID, query }, "", rejected) as {
					options: { label: string }[];
				}
			).options.map((option) => option.label);
		expect(labels("")).toEqual(["Lea"]);
		expect(labels("", "s-4")).toEqual(["Anna", "Lea"]);
		expect(labels("ann")).toEqual(["Anna", "ann"]);
		// Typed, no longer a suggestion: no reason, no strength.
		expect(
			(
				answerSpeakerOptions(
					recorded,
					{ speakerID: "s-3", query: "ann" },
					"",
					rejected,
				) as { options: object[] }
			).options[0],
		).toEqual({ kind: "person", label: "Anna", personID: "p-anna" });
	});

	it("quotes what a clip plays, else gives the voice's nearby words", async () => {
		const partly = await detail("speakers-partly");
		const speaker4 = partly.speakers.find((row) => row.id === SPEAKER_4);
		// Speaker 4 has no clip: nothing plays, so nothing is quoted.
		expect(speaker4?.quote).toBeUndefined();
		expect(speaker4?.context).toBeTruthy();
		const withClip = partly.speakers.filter((row) => row.hasClip);
		expect(withClip.every((row) => row.quote && !row.context)).toBe(true);
		const near = await detail("speakers-context");
		expect(near.speakers.every((row) => !row.quote && row.context)).toBe(true);
	});

	it("makes every voice not a person", async () => {
		const meeting = await detail("speakers-not-people");
		expect(meeting.speakers.length).toBeGreaterThan(0);
		expect(meeting.speakers.every((row) => row.dismissed)).toBe(true);
	});

	it("reports the Diarize stage for the processing meeting", async () => {
		const snapshots = applyScenario(
			await loadFixtureSnapshots(),
			new URLSearchParams("scenario=speakers-finding"),
		);
		const progress = snapshots.progress as ProgressSnapshot;
		const meeting = snapshots["meeting.detail"] as MeetingDetailSnapshot;
		expect(progress.entries).toEqual([
			expect.objectContaining({ meetingID: meeting.id, stage: "diarize" }),
		]);
	});
});

describe("fixtureKey", () => {
	it("maps a fixture path to its topic", () => {
		expect(fixtureKey("../../fixtures/bridge/meetings.list.json")).toBe(
			"meetings.list",
		);
		expect(fixtureKey("speakers.options.reply.json")).toBe(
			"speakers.options.reply",
		);
	});

	it("tells snapshots from replies, params and envelopes", () => {
		expect(isSnapshotKey("meetings.list")).toBe(true);
		expect(isSnapshotKey("settings.general")).toBe(true);
		for (const key of [
			"index",
			"speakers.options.reply",
			"params.meetingID",
			"envelope.request",
			"reply.confirm",
		]) {
			expect(isSnapshotKey(key), key).toBe(false);
		}
		expect(replyMethod("speakers.options.reply")).toBe("speakers.options");
		expect(replyMethod("meetings.list")).toBeNull();
	});
});

describe("createMockTransport", () => {
	it("emits the fixture snapshot to a subscriber, including late ones", async () => {
		const transport = createMockTransport({
			snapshots: { app: { version: "0.10.0" } },
		});
		await transport.ready();
		const handler = vi.fn();
		const unsubscribe = transport.subscribe("app", handler);
		expect(handler).toHaveBeenCalledWith({ version: "0.10.0" });

		transport.emit("app", { version: "0.10.1" });
		expect(handler).toHaveBeenLastCalledWith({ version: "0.10.1" });

		unsubscribe();
		transport.emit("app", { version: "0.10.2" });
		expect(handler).toHaveBeenCalledTimes(2);
	});

	it("loads snapshots from an async source", async () => {
		const transport = createMockTransport({
			snapshots: async () => ({ recording: { state: "idle" } }),
		});
		const handler = vi.fn();
		transport.subscribe("recording", handler);
		expect(handler).not.toHaveBeenCalled();
		await transport.ready();
		expect(handler).toHaveBeenCalledWith({ state: "idle" });
	});

	it("answers calls with recorded replies and records every call", async () => {
		const transport = createMockTransport({
			replies: { "speakers.options": [{ id: "s1", name: "Anna" }] },
		});
		const reply = await transport.call("speakers.options", { query: "an" });
		expect(reply).toEqual([{ id: "s1", name: "Anna" }]);
		const nothing = await transport.call("meetings.delete", { id: "m1" });
		expect(nothing).toBeUndefined();
		expect(transport.calls).toEqual([
			{ method: "speakers.options", params: { query: "an" } },
			{ method: "meetings.delete", params: { id: "m1" } },
		]);
	});

	it("rejects with a BridgeError when the recorded reply is an error", async () => {
		const transport = createMockTransport({
			replies: { "recording.start": new Error("microphone denied") },
		});
		await expect(transport.call("recording.start", {})).rejects.toBeInstanceOf(
			BridgeError,
		);
		await expect(transport.call("recording.start", {})).rejects.toMatchObject({
			method: "recording.start",
			code: "mock",
			message: "microphone denied",
		});
	});
});
