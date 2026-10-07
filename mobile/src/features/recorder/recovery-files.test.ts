import { beforeEach, describe, expect, it, vi } from "vitest";

/**
 * expo-file-system over a map of absolute URIs under the app's container,
 * which `moveContainer` moves the way iOS does on an app update or a restore.
 * As on iOS, `move` throws for a source outside the current container or
 * missing, and without `overwrite` when the target exists; `size` is null
 * for a missing file and for the paths in `sizeUnreadable`.
 */
const fs = vi.hoisted(() => ({
	files: new Map<string, string>(),
	dirs: new Set<string>(),
	container: "",
	sizeUnreadable: new Set<string>(),
}));

vi.mock("expo-file-system", () => {
	const join = (parts: unknown[]) =>
		parts
			.map((p) => (typeof p === "string" ? p : (p as { uri: string }).uri))
			.map((s, i) =>
				i === 0 ? s.replace(/\/+$/, "") : s.replace(/^\/+|\/+$/g, ""),
			)
			.join("/");
	class Directory {
		readonly uri: string;
		constructor(...parts: unknown[]) {
			this.uri = join(parts);
		}
		get exists() {
			return (
				fs.dirs.has(this.uri) ||
				[...fs.files.keys()].some((k) => k.startsWith(`${this.uri}/`))
			);
		}
		create() {
			fs.dirs.add(this.uri);
		}
		list() {
			return [...fs.files.keys()]
				.filter(
					(k) =>
						k.startsWith(`${this.uri}/`) &&
						!k.slice(this.uri.length + 1).includes("/"),
				)
				.map((k) => new File(k));
		}
	}
	class File {
		readonly uri: string;
		constructor(...parts: unknown[]) {
			this.uri = join(parts);
		}
		get name() {
			return this.uri.slice(this.uri.lastIndexOf("/") + 1);
		}
		get exists() {
			return fs.files.has(this.uri);
		}
		get size() {
			if (fs.sizeUnreadable.has(this.uri)) return null;
			return fs.files.get(this.uri)?.length ?? null;
		}
		get creationTime() {
			return 1_759_312_800_000;
		}
		get lastModified() {
			return null;
		}
		async text() {
			const text = fs.files.get(this.uri);
			if (text === undefined) throw new Error("missing");
			return text;
		}
		async bytes() {
			return new Uint8Array();
		}
		create() {
			fs.files.set(this.uri, "");
		}
		write(text: string) {
			fs.files.set(this.uri, text);
		}
		async move(target: File, options?: { overwrite?: boolean }) {
			if (!this.uri.startsWith(`${fs.container}/`)) {
				throw new Error(`MissingPermission: ${this.uri}`);
			}
			const text = fs.files.get(this.uri);
			if (text === undefined) throw new Error("No such file");
			if (fs.files.has(target.uri) && !options?.overwrite) {
				throw new Error("Destination already exists");
			}
			fs.files.delete(this.uri);
			fs.files.set(target.uri, text);
		}
	}
	return {
		File,
		Directory,
		Paths: {
			get document() {
				return new Directory(fs.container);
			},
		},
	};
});

vi.mock("@modules/steno-link/native", () => ({
	stenoLink: () => ({
		sha256: async (uri: string) =>
			`sha(${uri.slice(uri.lastIndexOf("/") + 1)})`,
	}),
}));

import {
	ensureQueueDirectory,
	expoQueueFiles,
	recorderDirectory,
} from "@/features/queue/queue-files";
import {
	addRecording,
	EMPTY_INDEX,
	type QueueIndex,
	setState,
} from "@/features/queue/queue-index";
import {
	createQueueStorage,
	serializeQueueIndex,
} from "@/features/queue/queue-storage";
import {
	CHUNK_SIZE,
	MIN_RECORDING_BYTES,
	recordingFileName,
} from "./recording-options";
import { applyRecovery, INTERRUPTED_MESSAGE, planRecovery } from "./recovery";
import { expoRecoveryFiles } from "./recovery-files";

const STARTED = "2026-10-01T10:00:00.000Z";
const OLD = "file:///var/mobile/Containers/Data/Application/AAAA/Documents";
const NEW = "file:///var/mobile/Containers/Data/Application/BBBB/Documents";
const ID = "11111111-1111-4111-8111-111111111111";
const QUEUE_NAME = recordingFileName(ID);
const RECORDER_NAME = "recording-ABCDEF01-2345-4678-89AB-CDEF01234567.m4a";
const AUDIO = "a".repeat(50_000);

const queuePath = (name: string) => `${fs.container}/queue/${name}`;
const recorderPath = (name: string) => `${fs.container}/ExpoAudio/${name}`;

/** Moves every file to the container at `to`, as iOS does. */
function moveContainer(to: string) {
	const from = fs.container;
	for (const [uri, text] of [...fs.files]) {
		fs.files.delete(uri);
		fs.files.set(uri.replace(from, to), text);
	}
	fs.dirs = new Set([...fs.dirs].map((d) => d.replace(from, to)));
	fs.container = to;
}

/** One launch: the queue load (`QueueProvider`), then crash recovery (`RecorderScreen`). */
async function launch() {
	const storage = createQueueStorage(
		expoQueueFiles,
		ensureQueueDirectory().uri,
		() => {},
		recorderDirectory().uri,
	);
	const loaded = await storage.load();
	const recovered = applyRecovery(
		loaded,
		await planRecovery(loaded, expoRecoveryFiles),
	);
	await storage.save(recovered);
	return recovered.recordings;
}

/**
 * A row the recorder added at start, writing to `RECORDER_NAME` in the old
 * container, left in `recording` by a crash or, by an earlier app version,
 * failed before it was hashed.
 */
function interrupted(state: "recording" | "failed"): QueueIndex {
	const index = addRecording(
		EMPTY_INDEX,
		{
			recordingID: ID,
			fileName: QUEUE_NAME,
			sourceUri: `${OLD}/ExpoAudio/${RECORDER_NAME}`,
			startedAt: STARTED,
			durationSeconds: 0,
			byteCount: 0,
			sha256: null,
			chunkSize: CHUNK_SIZE,
		},
		"recording",
	);
	return state === "recording"
		? index
		: setState(index, ID, "failed", { lastError: INTERRUPTED_MESSAGE });
}

beforeEach(() => {
	fs.files.clear();
	fs.dirs.clear();
	fs.sizeUnreadable.clear();
	fs.container = OLD;
});

describe("crash recovery over expo's files", () => {
	for (const state of ["recording", "failed"] as const) {
		for (const moved of [false, true]) {
			it(`queues a ${state} row's recorder file${moved ? " after the app's container moved" : ""}`, async () => {
				fs.files.set(
					queuePath("index.json"),
					serializeQueueIndex(interrupted(state)),
				);
				fs.files.set(recorderPath(RECORDER_NAME), AUDIO);
				if (moved) moveContainer(NEW);

				expect(await launch()).toMatchObject([
					{
						recordingID: ID,
						state: "queued",
						byteCount: AUDIO.length,
						sha256: `sha(${QUEUE_NAME})`,
					},
				]);
				expect(fs.files.get(queuePath(QUEUE_NAME))).toBe(AUDIO);
				expect(fs.files.has(recorderPath(RECORDER_NAME))).toBe(false);
				// The next launch adds no row.
				expect(await launch()).toMatchObject([{ state: "queued" }]);
			});
		}
	}

	it("keeps a queue file whose size cannot be read until a later launch reads it", async () => {
		fs.files.set(
			queuePath("index.json"),
			serializeQueueIndex(interrupted("recording")),
		);
		fs.files.set(queuePath(QUEUE_NAME), AUDIO);
		fs.files.set(recorderPath(RECORDER_NAME), "h".repeat(2_000));
		fs.sizeUnreadable.add(queuePath(QUEUE_NAME));

		expect(await launch()).toMatchObject([{ state: "recording" }]);
		expect(fs.files.get(queuePath(QUEUE_NAME))).toBe(AUDIO);

		fs.sizeUnreadable.clear();
		expect(await launch()).toMatchObject([
			{ state: "queued", byteCount: AUDIO.length },
		]);
		expect(fs.files.get(queuePath(QUEUE_NAME))).toBe(AUDIO);
	});
});

describe("expoRecoveryFiles", () => {
	it("reads a queue file's size: 0 when missing, null when it cannot be read", () => {
		expect(expoRecoveryFiles.size(QUEUE_NAME)).toBe(0);
		fs.files.set(queuePath(QUEUE_NAME), AUDIO);
		expect(expoRecoveryFiles.size(QUEUE_NAME)).toBe(AUDIO.length);
		fs.sizeUnreadable.add(queuePath(QUEUE_NAME));
		expect(expoRecoveryFiles.size(QUEUE_NAME)).toBeNull();
	});

	it("replaces a queue file only when it read as holding no meaningful audio", async () => {
		const source = `${OLD}/ExpoAudio/${RECORDER_NAME}`;
		fs.files.set(recorderPath(RECORDER_NAME), "h".repeat(2_000));
		for (const [kept, unreadable] of [
			["q".repeat(MIN_RECORDING_BYTES), false],
			["q".repeat(10), true],
		] as const) {
			fs.files.set(queuePath(QUEUE_NAME), kept);
			if (unreadable) fs.sizeUnreadable.add(queuePath(QUEUE_NAME));
			await expect(expoRecoveryFiles.adopt(source, QUEUE_NAME)).rejects.toThrow(
				/not replaced/,
			);
			expect(fs.files.get(queuePath(QUEUE_NAME))).toBe(kept);
		}
		fs.sizeUnreadable.clear();
		fs.files.set(queuePath(QUEUE_NAME), "ftyp header");
		await expoRecoveryFiles.adopt(source, QUEUE_NAME);
		expect(fs.files.get(queuePath(QUEUE_NAME))).toBe("h".repeat(2_000));
		expect(fs.files.has(recorderPath(RECORDER_NAME))).toBe(false);
	});
});
