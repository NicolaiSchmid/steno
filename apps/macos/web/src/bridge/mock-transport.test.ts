import { describe, expect, it, vi } from "vitest";
import {
	createMockTransport,
	fixtureKey,
	isSnapshotKey,
	replyMethod,
} from "./mock-transport";
import { BridgeError } from "./transport";

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
