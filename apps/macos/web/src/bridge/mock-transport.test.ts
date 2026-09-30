import { describe, expect, it, vi } from "vitest";
import {
	type MeetingDetailSnapshot,
	type MeetingsListSnapshot,
	type RecordingSnapshot,
	topicSchemas,
} from "./contract";
import {
	applyScenario,
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
		expect(replyMethods("reply.confirm")).toEqual([
			"ui.confirmDestructive",
			"meetings.delete",
			"meeting.deleteRecordingNow",
			"meeting.setKeepAudio",
		]);
		expect(replyMethods("speakers.options.reply")).toEqual([
			"speakers.options",
		]);
		expect(replyMethods("meetings.list")).toEqual([]);
		const replies = await loadFixtureReplies();
		expect(replies["ui.confirmDestructive"]).toEqual({ confirmed: true });
		expect(replies["meetings.delete"]).toEqual({ confirmed: true });
		expect(replies["meeting.setKeepAudio"]).toEqual({ confirmed: true });
		expect(replies["settings.export.chooseVault"]).toBeDefined();
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

	it("empties the list and drops the detail", async () => {
		const result = applyScenario(
			await loadFixtureSnapshots(),
			new URLSearchParams("scenario=empty"),
		);
		const list = topicSchemas["meetings.list"].parse(result["meetings.list"]);
		expect(list.groups).toEqual([]);
		expect(list.counts.all).toBe(0);
		expect(list.selection).toBeUndefined();
		expect(result["meeting.detail"]).toBeUndefined();
	});

	it("makes the live recording current, started 12:34 ago", async () => {
		const before = Date.now();
		const result = applyScenario(
			await loadFixtureSnapshots(),
			new URLSearchParams("scenario=recording"),
		);
		const recording = topicSchemas.recording.parse(
			result.recording,
		) as RecordingSnapshot;
		expect(recording.state).toBe("recording");
		const after = Date.now();
		const started = new Date(recording.startedAt ?? "").getTime();
		expect(after - started).toBeGreaterThanOrEqual(754_000);
		expect(before - started).toBeLessThanOrEqual(754_000);
	});

	it("keeps the live recording's auto-stop so the countdown shows", async () => {
		const result = applyScenario(
			await loadFixtureSnapshots(),
			new URLSearchParams("scenario=recording"),
		);
		const recording = topicSchemas.recording.parse(
			result.recording,
		) as RecordingSnapshot;
		expect(recording.autoStop).toEqual({
			reason: "Zoom closed",
			remainingSeconds: 42,
			totalSeconds: 60,
		});
	});

	it("denies the microphone while idle", async () => {
		const result = applyScenario(
			await loadFixtureSnapshots(),
			new URLSearchParams("scenario=denied"),
		);
		const recording = topicSchemas.recording.parse(
			result.recording,
		) as RecordingSnapshot;
		expect(recording.state).toBe("idle");
		expect(recording.deniedPermissions).toEqual(["microphone"]);
	});

	it("fails the selected meeting's export", async () => {
		const snapshots = await loadFixtureSnapshots();
		const result = applyScenario(
			snapshots,
			new URLSearchParams("scenario=export-failed"),
		);
		const detail = topicSchemas["meeting.detail"].parse(
			result["meeting.detail"],
		) as MeetingDetailSnapshot;
		expect(detail.id).toBe(
			(snapshots["meeting.detail"] as MeetingDetailSnapshot).id,
		);
		expect(detail.export.status).toBe("failed");
		expect(detail.export.canReexport).toBe(true);
		expect(detail.export.message).toContain("Failed");
	});

	it("selects the failed meeting with a failed detail", async () => {
		const result = applyScenario(
			await loadFixtureSnapshots(),
			new URLSearchParams("scenario=failed&tab=transcript"),
		);
		const list = topicSchemas["meetings.list"].parse(
			result["meetings.list"],
		) as MeetingsListSnapshot;
		const detail = topicSchemas["meeting.detail"].parse(
			result["meeting.detail"],
		) as MeetingDetailSnapshot;
		expect(list.selection).toBe("00000000-0000-0000-0000-000000000044");
		expect(detail.id).toBe(list.selection);
		expect(detail.state).toBe("failed");
		expect(detail.failureReason).toContain("Transcription failed");
		expect(detail.canRerunSummary).toBe(true);
		expect(detail.tab).toBe("transcript");
	});

	it("adds and selects the meeting the progress entry names", async () => {
		const snapshots = await loadFixtureSnapshots();
		const result = applyScenario(
			snapshots,
			new URLSearchParams("scenario=processing"),
		);
		const list = topicSchemas["meetings.list"].parse(
			result["meetings.list"],
		) as MeetingsListSnapshot;
		const detail = topicSchemas["meeting.detail"].parse(
			result["meeting.detail"],
		) as MeetingDetailSnapshot;
		const progress = topicSchemas.progress.parse(snapshots.progress);
		expect(list.selection).toBe(progress.entries[0]?.meetingID);
		expect(list.groups[0]?.meetings[0]?.state).toBe("processing");
		expect(list.counts.processing).toBe(1);
		expect(detail.id).toBe(list.selection);
		expect(detail.state).toBe("processing");
		expect(detail.summaryStatus.kind).toBe("pending");
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
