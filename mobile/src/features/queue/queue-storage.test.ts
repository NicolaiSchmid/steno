import { describe, expect, it } from "vitest";

import { addRecording, EMPTY_INDEX, type QueueIndex } from "./queue-index";
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
	it("returns the empty index when nothing was saved", async () => {
		const files = memoryFiles();
		const storage = createQueueStorage(files.api, "file:///docs/queue/");
		expect(storage.indexPath).toBe("file:///docs/queue/index.json");
		expect(await storage.load()).toEqual(EMPTY_INDEX);
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
