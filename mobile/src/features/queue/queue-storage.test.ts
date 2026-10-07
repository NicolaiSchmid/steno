import { describe, expect, it } from "vitest";

import {
	CHUNK_SIZE,
	recordingFileName,
} from "@/features/recorder/recording-options";
import { applyRecovery, planRecovery } from "@/features/recorder/recovery";
import { planNext } from "@/features/sync/upload-coordinator";
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

/**
 * In-memory file API; `failRename` makes the next rename throw once. `list`
 * returns every file under the directory, its creation time from `created`
 * by file name (0 when absent).
 */
function memoryFiles(
	initial: Record<string, string> = {},
	created: Record<string, number> = {},
) {
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
		async list(directory) {
			calls.push(`list ${directory}`);
			const prefix = `${directory.replace(/\/+$/, "")}/`;
			return [...store.keys()]
				.filter((path) => path.startsWith(prefix))
				.map((path) => path.slice(prefix.length))
				.filter((name) => !name.includes("/"))
				.map((name) => ({ name, createdAt: created[name] ?? 0 }));
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
			"list file:///docs/queue/",
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

	it("sets a corrupt index aside", async () => {
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
		expect(files.calls).toEqual([
			"read file:///docs/queue/index.json",
			"list file:///docs/queue",
		]);
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

	it("rebuilds from the files when only a corrupt temp file is left and nothing can be quarantined", async () => {
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

const Q = "file:///docs/queue";
const A = "11111111-1111-4111-8111-111111111111";
const B = "22222222-2222-4222-8222-222222222222";
const A_STARTED = "2026-10-01T09:00:00.000Z";
const B_STARTED = "2026-10-01T10:00:00.000Z";
/** Creation times by file name, as the file system reports them. */
const CREATED = {
	[recordingFileName(A)]: Date.parse(A_STARTED),
	[recordingFileName(B)]: Date.parse(B_STARTED),
};

/** The row the storage adds for a recording file no row names. */
function adopted(recordingID: string, startedAt: string) {
	return {
		recordingID,
		fileName: recordingFileName(recordingID),
		sourceUri: null,
		startedAt,
		durationSeconds: 0,
		byteCount: 0,
		sha256: null,
		chunkSize: CHUNK_SIZE,
		uploadedChunks: [],
		state: "recording",
		attempts: 0,
		nextAttemptAt: null,
		lastError: null,
		meetingID: null,
	};
}

describe("recording files the index does not list", () => {
	it("rebuild a corrupt index, and the rebuilt rows are hashed, queued and uploaded oldest first", async () => {
		const logs: string[] = [];
		// B is listed first; the rows come out oldest first.
		const files = memoryFiles(
			{
				[`${Q}/index.json`]: "{oops",
				[`${Q}/${recordingFileName(B)}`]: "audio",
				[`${Q}/${recordingFileName(A)}`]: "audio",
			},
			CREATED,
		);
		const storage = createQueueStorage(files.api, Q, (m) => logs.push(m));
		const loaded = await storage.load();
		expect(loaded).toEqual({
			version: 1,
			recordings: [adopted(A, A_STARTED), adopted(B, B_STARTED)],
		});
		// The corrupt index is kept aside, never deleted, and no file is removed.
		expect(files.store.get(`${Q}/index.corrupt.json`)).toBe("{oops");
		expect(files.calls.filter((c) => c.startsWith("remove"))).toEqual([
			`remove ${Q}/index.corrupt.json`,
		]);
		expect(logs).toEqual([
			"queue index unreadable, rebuilding it from the recording files: QueueError: index is not JSON",
			"queue index had no row for 2 recording file(s), adopted them",
		]);

		// Launch recovery hashes and queues them; the planner announces the older.
		const patches = await planRecovery(loaded, {
			size: () => 80_000,
			adopt: async () => {
				throw new Error("nothing to move");
			},
			sha256: async (fileName) => `sha(${fileName})`,
		});
		const recovered = applyRecovery(loaded, patches);
		expect(recovered.recordings).toMatchObject([
			{
				recordingID: A,
				state: "queued",
				byteCount: 80_000,
				sha256: `sha(${recordingFileName(A)})`,
				durationSeconds: 10,
			},
			{ recordingID: B, state: "queued", byteCount: 80_000 },
		]);
		expect(
			planNext(recovered, true, new Set(), new Date("2026-10-02T00:00:00Z")),
		).toEqual({ kind: "announce", recordingID: A });

		// Saved, the rebuilt index loads back as it is.
		await storage.save(recovered);
		expect(await storage.load()).toEqual(recovered);
	});

	it("rebuild the index when the index and the temp file are both corrupt", async () => {
		const logs: string[] = [];
		const files = memoryFiles(
			{
				[`${Q}/index.json`]: "{oops",
				[`${Q}/index.json.tmp`]: "nope",
				[`${Q}/${recordingFileName(A)}`]: "audio",
			},
			CREATED,
		);
		const storage = createQueueStorage(files.api, Q, (m) => logs.push(m));
		expect(await storage.load()).toEqual({
			version: 1,
			recordings: [adopted(A, A_STARTED)],
		});
		expect(files.store.get(`${Q}/index.corrupt.json`)).toBe("{oops");
		expect(files.store.has(`${Q}/${recordingFileName(A)}`)).toBe(true);
		expect(logs[0]).toMatch(/rebuilding it from the recording files/);
	});

	it("leave an empty queue when no recording file is on disk", async () => {
		const logs: string[] = [];
		const files = memoryFiles({
			[`${Q}/index.json`]: "{oops",
			[`${Q}/index.corrupt.json`]: "older",
			[`${Q}/notes.m4a`]: "not a recording",
		});
		const storage = createQueueStorage(files.api, Q, (m) => logs.push(m));
		expect(await storage.load()).toEqual(EMPTY_INDEX);
		expect(files.store.get(`${Q}/index.corrupt.json`)).toBe("{oops");
		expect(files.store.get(`${Q}/notes.m4a`)).toBe("not a recording");
		expect(logs).toEqual([
			"queue index unreadable, rebuilding it from the recording files: QueueError: index is not JSON",
		]);
	});

	it("join the rows of a temp file that missed a newer recording", async () => {
		const files = memoryFiles(
			{
				[`${Q}/index.json`]: '{"version":1,"recordings":[{"rec',
				[`${Q}/index.json.tmp`]: serializeQueueIndex(one),
				[`${Q}/a.m4a`]: "audio",
				[`${Q}/${recordingFileName(B)}`]: "audio",
			},
			CREATED,
		);
		const storage = createQueueStorage(files.api, Q);
		expect(await storage.load()).toEqual({
			version: 1,
			recordings: [...one.recordings, adopted(B, B_STARTED)],
		});
	});

	it("join a readable index too, and a file a row names keeps that row", async () => {
		// A was delivered but its delete never ran; B's row was never saved.
		const delivered = setState(
			addRecording(EMPTY_INDEX, {
				recordingID: A,
				fileName: recordingFileName(A),
				startedAt: A_STARTED,
				durationSeconds: 10,
				byteCount: 80_000,
				sha256: "HASH",
				chunkSize: CHUNK_SIZE,
			}),
			A,
			"delivered",
			{ meetingID: "m-1" },
		);
		const files = memoryFiles(
			{
				[`${Q}/index.json`]: serializeQueueIndex(delivered),
				[`${Q}/${recordingFileName(A)}`]: "audio",
				[`${Q}/${recordingFileName(B)}`]: "audio",
			},
			CREATED,
		);
		const storage = createQueueStorage(files.api, Q);
		expect(await storage.load()).toEqual({
			version: 1,
			recordings: [...delivered.recordings, adopted(B, B_STARTED)],
		});
	});

	it("leave the index as read when the directory cannot be listed", async () => {
		const logs: string[] = [];
		const files = memoryFiles({
			[`${Q}/index.json`]: serializeQueueIndex(one),
		});
		files.api.list = async () => {
			throw new Error("EACCES");
		};
		const storage = createQueueStorage(files.api, Q, (m) => logs.push(m));
		expect(await storage.load()).toEqual(one);
		expect(logs).toEqual([
			"queue directory unreadable, no recording files adopted: Error: EACCES",
		]);
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
