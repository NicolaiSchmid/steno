import { describe, expect, it } from "vitest";

import {
	addRecording,
	EMPTY_INDEX,
	type QueueIndex,
	setState,
} from "./queue-index";
import {
	createQueueStorage,
	parseQueueIndex,
	type QueueFileAPI,
	serializeQueueIndex,
} from "./queue-storage";

/** In-memory file API; `failRename` makes the next rename throw once. */
function memoryFiles(initial: Record<string, string> = {}) {
	const store = new Map(Object.entries(initial));
	const calls: string[] = [];
	let failRename = false;
	const api: QueueFileAPI = {
		async readText(path) {
			calls.push(`read ${path}`);
			return store.get(path) ?? null;
		},
		async writeText(path, text) {
			calls.push(`write ${path}`);
			store.set(path, text);
		},
		async rename(from, to) {
			calls.push(`rename ${from} -> ${to}`);
			if (failRename) {
				failRename = false;
				throw new Error("EIO rename");
			}
			const text = store.get(from);
			if (text === undefined) throw new Error(`missing ${from}`);
			store.delete(from);
			store.set(to, text);
		},
		async remove(path) {
			calls.push(`remove ${path}`);
			store.delete(path);
		},
	};
	return {
		api,
		store,
		calls,
		failNextRename() {
			failRename = true;
		},
	};
}

const one: QueueIndex = addRecording(EMPTY_INDEX, {
	recordingID: "a",
	fileName: "a.m4a",
	startedAt: "2026-09-25T09:00:00.000Z",
	durationSeconds: 1,
	byteCount: 2,
	sha256: null,
	chunkSize: 16,
});

describe("createQueueStorage", () => {
	it("round-trips the recorder's source URI and defaults it for indexes written without it", async () => {
		const withSource = addRecording(
			EMPTY_INDEX,
			{
				recordingID: "r",
				fileName: "r.m4a",
				sourceUri: "file:///docs/ExpoAudio/recording-1.m4a",
				startedAt: "2026-09-25T09:00:00.000Z",
				durationSeconds: 0,
				byteCount: 0,
				sha256: null,
				chunkSize: 16,
			},
			"recording",
		);
		expect(parseQueueIndex(serializeQueueIndex(withSource))).toEqual(
			withSource,
		);

		const legacy = JSON.parse(serializeQueueIndex(one)) as {
			recordings: Record<string, unknown>[];
		};
		delete legacy.recordings[0]?.sourceUri;
		expect(parseQueueIndex(JSON.stringify(legacy))).toEqual(one);
		expect(one.recordings[0]?.sourceUri).toBeNull();
	});

	it("returns the empty index when nothing was saved", async () => {
		const files = memoryFiles();
		const storage = createQueueStorage(files.api, "file:///docs/queue/");
		expect(await storage.load()).toEqual(EMPTY_INDEX);
		expect(files.calls).toEqual([
			"read file:///docs/queue/index.json",
			"read file:///docs/queue/index.json.tmp",
		]);
	});

	it("writes through a temp file and renames it over the index", async () => {
		const files = memoryFiles();
		const storage = createQueueStorage(files.api, "file:///docs/queue");
		await storage.save(one);
		expect(files.calls).toEqual([
			"write file:///docs/queue/index.json.tmp",
			"rename file:///docs/queue/index.json.tmp -> file:///docs/queue/index.json",
		]);
		expect(files.store.has("file:///docs/queue/index.json.tmp")).toBe(false);
		expect(await storage.load()).toEqual(one);
	});

	it("keeps the previous index intact when the rename throws", async () => {
		const files = memoryFiles();
		const storage = createQueueStorage(files.api, "file:///docs/queue");
		await storage.save(one);
		const two = addRecording(one, {
			recordingID: "b",
			fileName: "b.m4a",
			startedAt: "2026-09-25T10:00:00.000Z",
			durationSeconds: 1,
			byteCount: 2,
			sha256: null,
			chunkSize: 16,
		});
		files.failNextRename();
		await expect(storage.save(two)).rejects.toThrow("EIO rename");
		expect(files.store.has("file:///docs/queue/index.json.tmp")).toBe(false);
		expect(await storage.load()).toEqual(one);
	});

	it("recovers from a leftover temp file when the index is missing", async () => {
		const files = memoryFiles({
			"file:///docs/queue/index.json.tmp": serializeQueueIndex(one),
		});
		const storage = createQueueStorage(files.api, "file:///docs/queue");
		expect(await storage.load()).toEqual(one);
	});

	it("sets a corrupt index aside and starts empty", async () => {
		const logs: string[] = [];
		const files = memoryFiles({ "file:///docs/queue/index.json": "{oops" });
		const storage = createQueueStorage(files.api, "file:///docs/queue", (m) =>
			logs.push(m),
		);
		expect(await storage.load()).toEqual(EMPTY_INDEX);
		expect(files.store.get("file:///docs/queue/index.corrupt.json")).toBe(
			"{oops",
		);
		expect(files.store.has("file:///docs/queue/index.json")).toBe(false);
		expect(logs[0]).toMatch(/not JSON/);
	});

	it("prefers the index over a stale temp file when both exist", async () => {
		const files = memoryFiles({
			"file:///docs/queue/index.json": serializeQueueIndex(one),
			"file:///docs/queue/index.json.tmp": serializeQueueIndex(EMPTY_INDEX),
		});
		const storage = createQueueStorage(files.api, "file:///docs/queue");
		expect(await storage.load()).toEqual(one);
		expect(files.calls).toEqual(["read file:///docs/queue/index.json"]);
	});

	it("takes a complete temp file over a torn index and sets the torn one aside", async () => {
		const logs: string[] = [];
		const torn = '{"version":1,"recordings":[{"rec';
		const files = memoryFiles({
			"file:///docs/queue/index.json": torn,
			"file:///docs/queue/index.json.tmp": serializeQueueIndex(one),
		});
		const storage = createQueueStorage(files.api, "file:///docs/queue", (m) =>
			logs.push(m),
		);
		expect(await storage.load()).toEqual(one);
		expect(files.store.get("file:///docs/queue/index.corrupt.json")).toBe(torn);
		expect(files.store.has("file:///docs/queue/index.json")).toBe(false);
		expect(logs[0]).toMatch(/using the temp file/);
		// The next save writes a fresh index over the temp file as usual.
		await storage.save(one);
		expect(await storage.load()).toEqual(one);
	});

	it("quarantines a schema-invalid index, replacing an older quarantine", async () => {
		const invalid = serializeQueueIndex(one).replace('"queued"', '"paused"');
		const files = memoryFiles({
			"file:///docs/queue/index.json": invalid,
			"file:///docs/queue/index.corrupt.json": "older",
		});
		const storage = createQueueStorage(files.api, "file:///docs/queue");
		expect(await storage.load()).toEqual(EMPTY_INDEX);
		expect(files.store.get("file:///docs/queue/index.corrupt.json")).toBe(
			invalid,
		);
		// A save after the quarantine writes a fresh index that loads back.
		await storage.save(one);
		expect(await storage.load()).toEqual(one);
		expect(files.store.get("file:///docs/queue/index.corrupt.json")).toBe(
			invalid,
		);
	});

	it("starts empty when only a corrupt temp file is left and nothing can be quarantined", async () => {
		const logs: string[] = [];
		const files = memoryFiles({ "file:///docs/queue/index.json.tmp": "nope" });
		const storage = createQueueStorage(files.api, "file:///docs/queue", (m) =>
			logs.push(m),
		);
		await expect(storage.load()).resolves.toEqual(EMPTY_INDEX);
		expect(logs).toHaveLength(1);
		expect(files.store.has("file:///docs/queue/index.corrupt.json")).toBe(
			false,
		);
	});

	it("keeps delivered rows with their meeting id across save and load", async () => {
		const files = memoryFiles();
		const storage = createQueueStorage(files.api, "file:///docs/queue");
		const delivered = setState(
			setState(one, "a", "uploading"),
			"a",
			"delivered",
			{ meetingID: "m-1" },
		);
		await storage.save(delivered);
		const loaded = await storage.load();
		expect(loaded.recordings[0]).toMatchObject({
			recordingID: "a",
			state: "delivered",
			meetingID: "m-1",
		});
		expect(loaded).toEqual(delivered);
	});

	it("does not touch the index when the temp write itself fails", async () => {
		const files = memoryFiles();
		const storage = createQueueStorage(files.api, "file:///docs/queue");
		await storage.save(one);
		files.api.writeText = async () => {
			throw new Error("ENOSPC");
		};
		await expect(storage.save(EMPTY_INDEX)).rejects.toThrow("ENOSPC");
		expect(await storage.load()).toEqual(one);
	});
});

describe("parseQueueIndex", () => {
	it("round-trips a serialized index", () => {
		expect(parseQueueIndex(serializeQueueIndex(one))).toEqual(one);
	});

	it("rejects other versions, shapes and invalid rows", () => {
		expect(() => parseQueueIndex('{"version":2,"recordings":[]}')).toThrow(
			/version 2/,
		);
		expect(() => parseQueueIndex('{"version":1}')).toThrow(/not an array/);
		expect(() => parseQueueIndex("[]")).toThrow(/not an object|version/);
		const bad = serializeQueueIndex(one).replace('"queued"', '"paused"');
		expect(() => parseQueueIndex(bad)).toThrow(/invalid field/);
		const dupe = {
			version: 1,
			recordings: [one.recordings[0], one.recordings[0]],
		};
		expect(() => parseQueueIndex(JSON.stringify(dupe))).toThrow(/duplicate/);
	});
});
