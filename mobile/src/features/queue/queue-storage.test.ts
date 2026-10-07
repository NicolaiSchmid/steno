import { describe, expect, it } from "vitest";

import {
	CHUNK_SIZE,
	MIN_RECORDING_BYTES,
	recordingFileName,
} from "@/features/recorder/recording-options";
import { applyRecovery, planRecovery } from "@/features/recorder/recovery";
import { planNext } from "@/features/sync/upload-coordinator";
import {
	addRecording,
	EMPTY_INDEX,
	type QueuedRecording,
	type QueueIndex,
	setState,
} from "./queue-index";
import {
	createQueueStorage,
	parseQueueIndex,
	type QueueFileAPI,
	serializeQueueIndex,
	UndecodableTextError,
} from "./queue-storage";
import { createQueueStore, QUEUE_NOT_LOADED_MESSAGE } from "./queue-store";

/**
 * In-memory file API; `failNextRename` makes the next rename or move throw
 * once and `failReads` the next that many reads; reads of `unreadable` paths
 * throw, and `undecodable` paths read as bytes that are not text. `move` throws when the target exists. `list`
 * returns every file under the directory, its creation time from `created`
 * by file name (0 when absent) and its size from the text's length.
 */
function memoryFiles(
	initial: Record<string, string> = {},
	created: Record<string, number> = {},
) {
	const store = new Map(Object.entries(initial));
	const calls: string[] = [];
	const undecodable = new Set<string>();
	const unreadable = new Set<string>();
	let failRename = false;
	let failReads = 0;
	const api: QueueFileAPI = {
		async readText(path) {
			calls.push(`read ${path}`);
			if (failReads > 0) {
				failReads -= 1;
				throw new Error("EACCES read");
			}
			if (unreadable.has(path)) throw new Error("EACCES read");
			if (undecodable.has(path) && store.has(path)) {
				throw new UndecodableTextError(path, "not UTF-8");
			}
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
		async move(from, to) {
			calls.push(`move ${from} -> ${to}`);
			if (failRename) {
				failRename = false;
				throw new Error("EIO move");
			}
			const text = store.get(from);
			if (text === undefined) throw new Error(`missing ${from}`);
			if (store.has(to)) throw new Error(`exists ${to}`);
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
				.map((name) => ({
					name,
					createdAt: created[name] ?? 0,
					size: store.get(`${prefix}${name}`)?.length ?? 0,
				}));
		},
	};
	return {
		api,
		store,
		calls,
		undecodable,
		unreadable,
		failNextRename() {
			failRename = true;
		},
		failReads(count: number) {
			failReads = count;
		},
	};
}

/** A recording's bytes: `label`, padded to the smallest file the queue adopts. */
const audio = (label = "") => `audio ${label}`.padEnd(MIN_RECORDING_BYTES, ".");

const one: QueueIndex = addRecording(EMPTY_INDEX, {
	recordingID: "a",
	fileName: "a.m4a",
	startedAt: "2026-09-25T09:00:00.000Z",
	durationSeconds: 1,
	byteCount: 2,
	sha256: "HASH",
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

	it("sets a schema-invalid index aside, replacing an older copy", async () => {
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
		// A save after setting it aside writes a fresh index that loads back.
		await storage.save(one);
		expect(await storage.load()).toEqual(one);
		expect(files.store.get("file:///docs/queue/index.corrupt.json")).toBe(
			invalid,
		);
	});

	it("rebuilds from the files when only a corrupt temp file is left and there is no index to set aside", async () => {
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
function adopted(recordingID: string, startedAt: string): QueuedRecording {
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

describe("a load with recording files the index does not list", () => {
	it("rebuilds a corrupt index, and the rebuilt rows are hashed, queued and uploaded oldest first", async () => {
		const logs: string[] = [];
		// B is listed first; the rows come out oldest first.
		const files = memoryFiles(
			{
				[`${Q}/index.json`]: "{oops",
				[`${Q}/${recordingFileName(B)}`]: audio(),
				[`${Q}/${recordingFileName(A)}`]: audio(),
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
		expect(files.calls.filter((c) => c.startsWith("remove"))).toEqual([]);
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

	it("rebuilds the index when the index and the temp file are both corrupt", async () => {
		const logs: string[] = [];
		const files = memoryFiles(
			{
				[`${Q}/index.json`]: "{oops",
				[`${Q}/index.json.tmp`]: "nope",
				[`${Q}/${recordingFileName(A)}`]: audio(),
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

	it("leaves an empty queue when no file on disk holds a recording", async () => {
		const logs: string[] = [];
		const files = memoryFiles({
			[`${Q}/index.json`]: "{oops",
			[`${Q}/index.corrupt.json`]: "older",
			[`${Q}/notes.m4a`]: "not a recording",
			// A header with no audio after it.
			[`${Q}/${recordingFileName(A)}`]: "ftyp header",
		});
		const storage = createQueueStorage(files.api, Q, (m) => logs.push(m));
		expect(await storage.load()).toEqual(EMPTY_INDEX);
		expect(files.store.get(`${Q}/index.corrupt.json`)).toBe("{oops");
		expect(files.store.get(`${Q}/notes.m4a`)).toBe("not a recording");
		expect(files.store.get(`${Q}/${recordingFileName(A)}`)).toBe("ftyp header");
		expect(logs).toEqual([
			"queue index unreadable, rebuilding it from the recording files: QueueError: index is not JSON",
		]);
	});

	it("joins the rows of a temp file that missed a newer recording", async () => {
		const files = memoryFiles(
			{
				[`${Q}/index.json`]: '{"version":1,"recordings":[{"rec',
				[`${Q}/index.json.tmp`]: serializeQueueIndex(one),
				[`${Q}/a.m4a`]: audio(),
				[`${Q}/${recordingFileName(B)}`]: audio(),
			},
			CREATED,
		);
		const storage = createQueueStorage(files.api, Q);
		expect(await storage.load()).toEqual({
			version: 1,
			recordings: [...one.recordings, adopted(B, B_STARTED)],
		});
	});

	it("joins a readable index too, and a file a row names keeps that row", async () => {
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
				[`${Q}/${recordingFileName(A)}`]: audio(),
				[`${Q}/${recordingFileName(B)}`]: audio(),
			},
			CREATED,
		);
		const storage = createQueueStorage(files.api, Q);
		expect(await storage.load()).toEqual({
			version: 1,
			recordings: [...delivered.recordings, adopted(B, B_STARTED)],
		});
	});

	it("leaves the index as read when the directory cannot be listed", async () => {
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

const REC = "file:///docs/ExpoAudio";
// expo-audio names its files after an upper-case UUID.
const C = "33333333-3333-4333-8333-333333333333";
const D = "44444444-4444-4444-8444-444444444444";
const E = "55555555-5555-4555-8555-555555555555";
const recorderName = (id: string) => `recording-${id.toUpperCase()}.m4a`;
const R = "77777777-7777-4777-8777-777777777777";
const Z = "88888888-8888-4888-8888-888888888888";
const C_STARTED = "2026-10-01T11:00:00.000Z";

/** Crash recovery over the memory files: moves by the source's file name. */
function recoveryFiles(files: ReturnType<typeof memoryFiles>) {
	return {
		size: (fileName: string) =>
			files.store.get(`${Q}/${fileName}`)?.length ?? 0,
		adopt: (sourceUri: string, fileName: string) =>
			files.api.move(
				`${REC}/${sourceUri.slice(sourceUri.lastIndexOf("/") + 1)}`,
				`${Q}/${fileName}`,
			),
		sha256: async (fileName: string) => `sha(${fileName})`,
	};
}

/** A row the recorder added at start, with expo-audio's file as its source. */
function started(recordingID: string, startedAt: string, source: string) {
	return addRecording(
		EMPTY_INDEX,
		{
			recordingID,
			fileName: recordingFileName(recordingID),
			sourceUri: `${REC}/${source}`,
			startedAt,
			durationSeconds: 0,
			byteCount: 0,
			sha256: null,
			chunkSize: CHUNK_SIZE,
		},
		"recording",
	).recordings[0] as QueuedRecording;
}

const withRows = (...rows: QueuedRecording[]): QueueIndex => ({
	version: 1,
	recordings: rows,
});

describe("recordings left in the recorder's directory", () => {
	it("move into the queue under the UUID of their name when no row will recover them", async () => {
		const logs: string[] = [];
		// B was recording when the app died: crash recovery moves its file.
		const index = withRows({
			...started(B, B_STARTED, recorderName(E)),
			sourceUri: `file:///private/docs/ExpoAudio/${recorderName(E)}`,
		});
		const files = memoryFiles(
			{
				[`${Q}/index.json`]: serializeQueueIndex(index),
				// C: the app died before its row was saved.
				[`${REC}/${recorderName(C)}`]: audio("C"),
				[`${REC}/${recorderName(E)}`]: audio("E"),
				// Created empty, or holding only the header, for a recording
				// that never started.
				[`${REC}/recording-66666666-6666-4666-8666-666666666666.m4a`]: "",
				[`${REC}/recording-99999999-9999-4999-8999-999999999999.m4a`]:
					"ftyp header",
				[`${REC}/notes.m4a`]: "not a recording",
			},
			{ [recorderName(C)]: Date.parse(C_STARTED) },
		);
		const storage = createQueueStorage(files.api, Q, (m) => logs.push(m), REC);
		const loaded = await storage.load();
		expect(loaded).toEqual(
			withRows(...index.recordings, adopted(C, C_STARTED)),
		);
		expect(files.store.get(`${Q}/${recordingFileName(C)}`)).toBe(audio("C"));
		expect([...files.store.keys()].filter((k) => k.startsWith(REC))).toEqual([
			`${REC}/${recorderName(E)}`,
			`${REC}/recording-66666666-6666-4666-8666-666666666666.m4a`,
			`${REC}/recording-99999999-9999-4999-8999-999999999999.m4a`,
			`${REC}/notes.m4a`,
		]);
		expect(logs).toEqual([
			"moved 1 recording file(s) without a row from the recorder's directory",
			"queue index had no row for 1 recording file(s), adopted them",
		]);

		// Launch recovery hashes and queues the moved file; B's comes from
		// the recorder's directory as before.
		const patches = await planRecovery(loaded, recoveryFiles(files));
		expect(applyRecovery(loaded, patches).recordings).toMatchObject([
			{ recordingID: B, state: "queued", byteCount: MIN_RECORDING_BYTES },
			{
				recordingID: C,
				state: "queued",
				sha256: `sha(${recordingFileName(C)})`,
			},
		]);
	});

	it("stay where they are when the queue already has their name, or the move fails", async () => {
		const logs: string[] = [];
		const files = memoryFiles({
			[`${Q}/index.json`]: serializeQueueIndex(EMPTY_INDEX),
			[`${Q}/${recordingFileName(C)}`]: audio("older C"),
			[`${REC}/${recorderName(C)}`]: audio("C"),
			[`${REC}/${recorderName(D)}`]: audio("D"),
		});
		files.failNextRename();
		const storage = createQueueStorage(files.api, Q, (m) => logs.push(m), REC);
		expect(await storage.load()).toEqual(
			withRows(adopted(C, new Date(0).toISOString())),
		);
		expect(files.store.get(`${Q}/${recordingFileName(C)}`)).toBe(
			audio("older C"),
		);
		expect(files.store.get(`${REC}/${recorderName(C)}`)).toBe(audio("C"));
		expect(files.store.get(`${REC}/${recorderName(D)}`)).toBe(audio("D"));
		expect(logs[0]).toBe(
			`recording ${recorderName(D)} not moved into the queue: Error: EIO move`,
		);

		// The next load moves it.
		expect((await storage.load()).recordings.map((r) => r.recordingID)).toEqual(
			[C, D],
		);
		expect(files.store.get(`${Q}/${recordingFileName(D)}`)).toBe(audio("D"));
	});

	it("never replace a queue file the listing missed", async () => {
		const logs: string[] = [];
		const files = memoryFiles({
			[`${Q}/index.json`]: serializeQueueIndex(EMPTY_INDEX),
			[`${Q}/${recordingFileName(C)}`]: audio("older C"),
			[`${REC}/${recorderName(C)}`]: audio("C"),
		});
		const list = files.api.list;
		files.api.list = async (directory) =>
			(await list(directory)).filter((e) => e.name !== recordingFileName(C));
		const storage = createQueueStorage(files.api, Q, (m) => logs.push(m), REC);
		expect(await storage.load()).toEqual(EMPTY_INDEX);
		expect(files.store.get(`${Q}/${recordingFileName(C)}`)).toBe(
			audio("older C"),
		);
		expect(files.store.get(`${REC}/${recorderName(C)}`)).toBe(audio("C"));
		expect(logs).toEqual([
			`recording ${recorderName(C)} not moved into the queue: Error: exists ${Q}/${recordingFileName(C)}`,
		]);
	});
});

describe("rows that failed before they were hashed", () => {
	it("go through crash recovery again under their own id, so no second row appears and no upload stalls", async () => {
		const logs: string[] = [];
		// R failed with its file still in the recorder's directory; A is
		// queued behind it; Z was queued by a Retry with no hash and no file.
		const index = withRows(
			{
				...started(R, A_STARTED, recorderName(C)),
				state: "failed",
				lastError: "Recording error",
			},
			{
				...started(A, B_STARTED, recorderName(D)),
				sourceUri: null,
				byteCount: MIN_RECORDING_BYTES,
				sha256: "HASH-A",
				state: "queued",
			},
			{
				...started(Z, C_STARTED, recorderName(E)),
				state: "queued",
				attempts: 2,
			},
		);
		const files = memoryFiles(
			{
				[`${Q}/index.json`]: serializeQueueIndex(index),
				[`${Q}/${recordingFileName(A)}`]: audio("A"),
				[`${REC}/${recorderName(C)}`]: audio("R"),
			},
			{ [recorderName(C)]: Date.parse(A_STARTED) },
		);
		const storage = createQueueStorage(files.api, Q, (m) => logs.push(m), REC);
		const loaded = await storage.load();
		const [r, a, z] = index.recordings as [
			QueuedRecording,
			QueuedRecording,
			QueuedRecording,
		];
		expect(loaded).toEqual(
			withRows({ ...r, state: "recording", lastError: null }, a, {
				...z,
				state: "recording",
				attempts: 0,
			}),
		);
		expect(files.store.get(`${REC}/${recorderName(C)}`)).toBe(audio("R"));
		expect(logs).toEqual([
			"2 recording(s) never hashed go through crash recovery again",
		]);

		// Recovery moves R's file under R and queues it; Z has no file.
		const recovered = applyRecovery(
			loaded,
			await planRecovery(loaded, recoveryFiles(files)),
		);
		expect(recovered.recordings).toMatchObject([
			{
				recordingID: R,
				state: "queued",
				sha256: `sha(${recordingFileName(R)})`,
			},
			{ recordingID: A, state: "queued" },
			{ recordingID: Z, state: "failed" },
		]);
		expect(files.store.get(`${Q}/${recordingFileName(R)}`)).toBe(audio("R"));
		expect([...files.store.keys()].filter((k) => k.startsWith(REC))).toEqual(
			[],
		);
		const now = new Date("2026-10-02T00:00:00Z");
		expect(planNext(recovered, true, new Set(), now)).toEqual({
			kind: "announce",
			recordingID: R,
		});
	});

	it("stay failed when no file of their own holds audio, and so does a failed row with a hash", async () => {
		const failedRow = (
			recordingID: string,
			startedAt: string,
			sha256: string | null,
		): QueuedRecording => ({
			...started(recordingID, startedAt, recorderName(recordingID)),
			sourceUri: null,
			sha256,
			state: "failed",
			lastError: "kept",
		});
		const index = withRows(
			// The hash threw after the move: its file is in the queue.
			failedRow(A, A_STARTED, null),
			// The file was empty.
			failedRow(B, B_STARTED, null),
			// The Mac kept refusing it.
			failedRow(C, C_STARTED, "HASH-C"),
			// An earlier app version recorded D and then E into one file, which
			// E's row, interrupted, recovers.
			{
				...failedRow(D, C_STARTED, null),
				sourceUri: `${REC}/${recorderName(E)}`,
			},
			started(E, C_STARTED, recorderName(E)),
		);
		const files = memoryFiles({
			[`${Q}/index.json`]: serializeQueueIndex(index),
			[`${Q}/${recordingFileName(A)}`]: audio("A"),
			[`${Q}/${recordingFileName(B)}`]: "",
			[`${Q}/${recordingFileName(C)}`]: audio("C"),
			[`${REC}/${recorderName(E)}`]: audio("E"),
		});
		const loaded = await createQueueStorage(files.api, Q, () => {}, REC).load();
		expect(
			loaded.recordings.map((row) => [
				row.recordingID,
				row.state,
				row.lastError,
			]),
		).toEqual([
			[A, "recording", null],
			[B, "failed", "kept"],
			[C, "failed", "kept"],
			[D, "failed", "kept"],
			[E, "recording", null],
		]);
		expect(files.store.get(`${REC}/${recorderName(E)}`)).toBe(audio("E"));
	});
});

describe("a queue load that fails", () => {
	it("does not fail when the corrupt index cannot be set aside", async () => {
		const logs: string[] = [];
		const files = memoryFiles(
			{
				[`${Q}/index.json`]: "{oops",
				[`${Q}/${recordingFileName(A)}`]: audio(),
			},
			CREATED,
		);
		files.api.remove = async () => {
			throw new Error("EPERM remove");
		};
		files.failNextRename();
		const storage = createQueueStorage(files.api, Q, (m) => logs.push(m));
		expect(await storage.load()).toEqual({
			version: 1,
			recordings: [adopted(A, A_STARTED)],
		});
		expect(logs[1]).toBe("queue index not set aside: Error: EIO rename");
	});

	it("rebuilds from the files when the temp file cannot be read, and keeps it aside", async () => {
		const logs: string[] = [];
		const temp = serializeQueueIndex(one);
		const files = memoryFiles(
			{
				[`${Q}/index.json`]: "{oops",
				[`${Q}/index.json.tmp`]: temp,
				[`${Q}/${recordingFileName(A)}`]: audio(),
			},
			CREATED,
		);
		files.unreadable.add(`${Q}/index.json.tmp`);
		const store = createQueueStore(
			createQueueStorage(files.api, Q, (m) => logs.push(m)),
		);
		await store.load();
		expect(store.snapshot()).toEqual({
			index: withRows(adopted(A, A_STARTED)),
			ready: true,
			loadError: null,
		});
		// Nothing a save writes later replaces what it may hold.
		expect(files.store.get(`${Q}/index.unreadable.json`)).toBe(temp);
		expect(files.store.has(`${Q}/index.json.tmp`)).toBe(false);
		expect(files.store.get(`${Q}/index.corrupt.json`)).toBe("{oops");
		expect(logs).toEqual([
			"queue index unreadable, rebuilding it from the recording files: QueueError: index is not JSON",
			"queue index had no row for 1 recording file(s), adopted them",
		]);
	});

	it("sets an index whose bytes are not text aside like a corrupt one", async () => {
		const logs: string[] = [];
		const files = memoryFiles(
			{
				[`${Q}/index.json`]: "\uFFFD",
				[`${Q}/index.json.tmp`]: "\uFFFD",
				[`${Q}/${recordingFileName(A)}`]: audio(),
			},
			CREATED,
		);
		files.undecodable.add(`${Q}/index.json`);
		files.undecodable.add(`${Q}/index.json.tmp`);
		const storage = createQueueStorage(files.api, Q, (m) => logs.push(m));
		expect(await storage.load()).toEqual(withRows(adopted(A, A_STARTED)));
		expect(files.store.get(`${Q}/index.corrupt.json`)).toBe("\uFFFD");
		expect(logs[0]).toBe(
			`queue index unreadable, rebuilding it from the recording files: UndecodableTextError: ${Q}/index.json is not text: not UTF-8`,
		);
	});

	it("saves nothing until a load succeeds, so the index keeps every row", async () => {
		const two = addRecording(one, {
			recordingID: B,
			fileName: recordingFileName(B),
			startedAt: B_STARTED,
			durationSeconds: 5,
			byteCount: 40_000,
			sha256: "HASH-B",
			chunkSize: CHUNK_SIZE,
		});
		const persisted = serializeQueueIndex(two);
		const files = memoryFiles({ [`${Q}/index.json`]: persisted });
		const logs: string[] = [];
		const store = createQueueStore(createQueueStorage(files.api, Q), (m) =>
			logs.push(m),
		);
		const next = {
			recordingID: E,
			fileName: recordingFileName(E),
			startedAt: C_STARTED,
			durationSeconds: 0,
			byteCount: 0,
			sha256: null,
			chunkSize: CHUNK_SIZE,
		};

		// The index cannot be read at launch, nor on the update's retry.
		files.failReads(2);
		await store.load();
		expect(store.snapshot()).toEqual({
			index: EMPTY_INDEX,
			ready: false,
			loadError: "EACCES read",
		});
		await expect(
			store.update((current) => addRecording(current, next, "recording")),
		).rejects.toThrow(QUEUE_NOT_LOADED_MESSAGE);
		expect(files.store.get(`${Q}/index.json`)).toBe(persisted);
		expect(files.calls.filter((c) => c.startsWith("write"))).toEqual([]);
		expect(logs).toHaveLength(2);

		// Readable again: the update sees and keeps every row.
		await store.update((current) => addRecording(current, next, "recording"));
		expect(store.snapshot()).toMatchObject({ ready: true, loadError: null });
		const saved = parseQueueIndex(files.store.get(`${Q}/index.json`) ?? "");
		expect(saved.recordings.map((r) => r.recordingID)).toEqual(["a", B, E]);
		expect(saved.recordings.slice(0, 2)).toEqual(two.recordings);
	});

	it("is tried again by load, and a loaded store does not load again", async () => {
		const files = memoryFiles({
			[`${Q}/index.json`]: serializeQueueIndex(one),
		});
		const store = createQueueStore(createQueueStorage(files.api, Q));
		const seen: boolean[] = [];
		store.subscribe(() => seen.push(store.snapshot().ready));
		files.failReads(1);
		await store.load();
		await store.load();
		expect(store.snapshot()).toEqual({
			index: one,
			ready: true,
			loadError: null,
		});
		expect(seen).toEqual([false, true]);
		const reads = files.calls.length;
		await store.load();
		expect(files.calls).toHaveLength(reads);
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
