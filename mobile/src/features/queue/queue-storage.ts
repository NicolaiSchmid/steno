import {
	CHUNK_SIZE,
	recorderFileID,
	recordingFileName,
	recordingIDFromFileName,
} from "@/features/recorder/recording-options";
import {
	addRecording,
	EMPTY_INDEX,
	type QueuedRecording,
	QueueError,
	type QueueIndex,
	SYNC_STATES,
	type SyncState,
} from "./queue-index";

/**
 * Persistence for the queue index through an injected file API, so vitest
 * can make `rename` throw (plan P4). Writes go to `index.json.tmp` first and
 * are renamed over `index.json`; a rename failure leaves the previous index
 * untouched and removes the temp file. Every load also lists the directory
 * and adds a row for each recording file no row names, so an index that was
 * lost, torn or written stale never strands a recording on disk; before
 * that it moves in the recordings a crash left in the recorder's directory
 * with no row to recover them (`adoptRecorderFiles`).
 *
 * A load that throws (the index could not be read) persists nothing and
 * leaves every file where it was; `queue-store.ts` then refuses to save
 * until a load succeeds.
 */
export type QueueFileAPI = {
	/** Null when the file does not exist. */
	readText(path: string): Promise<string | null>;
	/** Creates or truncates. */
	writeText(path: string, text: string): Promise<void>;
	/** Replaces `to` if it exists. */
	rename(from: string, to: string): Promise<void>;
	/** No-op when missing. */
	remove(path: string): Promise<void>;
	/** The files directly in `directory`; none when it does not exist. */
	list(directory: string): Promise<QueueDirectoryEntry[]>;
};

export type QueueDirectoryEntry = {
	name: string;
	/** Milliseconds since the epoch. */
	createdAt: number;
	/** Bytes. */
	size: number;
};

export type QueueStorage = {
	load(): Promise<QueueIndex>;
	save(index: QueueIndex): Promise<void>;
};

function isString(v: unknown): v is string {
	return typeof v === "string";
}
function isNullableString(v: unknown): v is string | null {
	return v === null || typeof v === "string";
}
function isNumber(v: unknown): v is number {
	return typeof v === "number" && Number.isFinite(v);
}

function parseRecording(raw: unknown, position: number): QueuedRecording {
	if (typeof raw !== "object" || raw === null) {
		throw new QueueError(`recording ${position} is not an object`);
	}
	const r = raw as Record<string, unknown>;
	const ok =
		isString(r.recordingID) &&
		isString(r.fileName) &&
		(r.sourceUri === undefined || isNullableString(r.sourceUri)) &&
		isString(r.startedAt) &&
		isNumber(r.durationSeconds) &&
		isNumber(r.byteCount) &&
		isNullableString(r.sha256) &&
		isNumber(r.chunkSize) &&
		Array.isArray(r.uploadedChunks) &&
		r.uploadedChunks.every((c) => Number.isInteger(c)) &&
		isString(r.state) &&
		SYNC_STATES.includes(r.state as SyncState) &&
		isNumber(r.attempts) &&
		isNullableString(r.nextAttemptAt) &&
		isNullableString(r.lastError) &&
		isNullableString(r.meetingID);
	if (!ok) throw new QueueError(`recording ${position} has an invalid field`);
	return {
		recordingID: r.recordingID as string,
		fileName: r.fileName as string,
		// Absent in indexes written before the column existed.
		sourceUri: (r.sourceUri as string | null | undefined) ?? null,
		startedAt: r.startedAt as string,
		durationSeconds: r.durationSeconds as number,
		byteCount: r.byteCount as number,
		sha256: r.sha256 as string | null,
		chunkSize: r.chunkSize as number,
		uploadedChunks: r.uploadedChunks as number[],
		state: r.state as SyncState,
		attempts: r.attempts as number,
		nextAttemptAt: r.nextAttemptAt as string | null,
		lastError: r.lastError as string | null,
		meetingID: r.meetingID as string | null,
	};
}

export function parseQueueIndex(text: string): QueueIndex {
	let parsed: unknown;
	try {
		parsed = JSON.parse(text);
	} catch {
		throw new QueueError("index is not JSON");
	}
	if (typeof parsed !== "object" || parsed === null) {
		throw new QueueError("index is not an object");
	}
	const record = parsed as Record<string, unknown>;
	if (record.version !== 1) {
		throw new QueueError(`unsupported index version ${String(record.version)}`);
	}
	if (!Array.isArray(record.recordings)) {
		throw new QueueError("index.recordings is not an array");
	}
	const recordings = record.recordings.map(parseRecording);
	const ids = new Set(recordings.map((r) => r.recordingID));
	if (ids.size !== recordings.length) {
		throw new QueueError("index has duplicate recording ids");
	}
	return { version: 1, recordings };
}

export function serializeQueueIndex(index: QueueIndex): string {
	return JSON.stringify(index, null, "\t");
}

function tryParse(
	text: string,
): { ok: true; value: QueueIndex } | { ok: false; reason: string } {
	try {
		return { ok: true, value: parseQueueIndex(text) };
	} catch (error) {
		return { ok: false, reason: String(error) };
	}
}

/**
 * `index` plus a row for every recording file in `entries` that no row names
 * by id or file name, oldest first. A row the index has keeps its state, so a
 * file left behind by a `delivered` row is not uploaded again. A new row is
 * `recording` with no size and no hash, the state crash recovery
 * (`planRecovery` in `src/features/recorder/recovery.ts`) picks up at launch:
 * it hashes the file and queues it, or marks the row failed when the file is
 * empty. `startedAt` is the file's creation time, which is when expo-audio
 * opened it for the recording; the move into the queue directory keeps it.
 */
export function adoptRecordingFiles(
	index: QueueIndex,
	entries: readonly QueueDirectoryEntry[],
): QueueIndex {
	const known = new Set(
		index.recordings.flatMap((r) => [r.recordingID, r.fileName]),
	);
	return entries
		.flatMap((entry) => {
			const recordingID = recordingIDFromFileName(entry.name);
			return recordingID && !known.has(recordingID) && !known.has(entry.name)
				? [{ recordingID, entry }]
				: [];
		})
		.sort((a, b) => a.entry.createdAt - b.entry.createdAt)
		.reduce(
			(acc, { recordingID, entry }) =>
				addRecording(
					acc,
					{
						recordingID,
						fileName: entry.name,
						startedAt: new Date(entry.createdAt).toISOString(),
						durationSeconds: 0,
						byteCount: 0,
						sha256: null,
						chunkSize: CHUNK_SIZE,
					},
					"recording",
				),
			index,
		);
}

/** A recording file to move from the recorder's directory into the queue. */
export type RecorderAdoption = {
	from: QueueDirectoryEntry;
	/** The queue file name, `<id>.m4a` with the id from the recorder's name. */
	to: string;
};

/**
 * The recordings in the recorder's directory (`recorderEntries`) that no
 * crash recovery will move: a file is left in place when it is not named
 * like an expo-audio recording, holds no bytes (no audio; the recorder's
 * file for the next recording is created empty), or a `recording` row names
 * it as its `sourceUri` (`planRecovery` moves it under that row). Every
 * other one, a crash before its row was saved or a row that failed with its
 * file still there, becomes a new queue file named after the UUID in the
 * recorder's name, unless a row or a queue file already has that name.
 */
export function adoptRecorderFiles(
	index: QueueIndex,
	queueEntries: readonly QueueDirectoryEntry[],
	recorderEntries: readonly QueueDirectoryEntry[],
): RecorderAdoption[] {
	const recovered = new Set(
		index.recordings.flatMap((r) =>
			r.state === "recording" && r.sourceUri
				? [r.sourceUri.slice(r.sourceUri.lastIndexOf("/") + 1)]
				: [],
		),
	);
	const taken = new Set([
		...index.recordings.flatMap((r) => [r.recordingID, r.fileName]),
		...queueEntries.map((e) => e.name),
	]);
	return recorderEntries.flatMap((from) => {
		const id = recorderFileID(from.name);
		if (!id || from.size <= 0 || recovered.has(from.name)) return [];
		const to = recordingFileName(id);
		if (taken.has(id) || taken.has(to)) return [];
		taken.add(to);
		return [{ from, to }];
	});
}

function join(directory: string, name: string): string {
	return `${directory.replace(/\/+$/, "")}/${name}`;
}

/**
 * `recorderDirectory` is expo-audio's directory (`Documents/ExpoAudio/`);
 * null leaves it alone. Loads run only while nothing records: the recorder
 * starts only once the queue is loaded (`RecorderScreen`), and a file of a
 * process that died has no writer left.
 */
export function createQueueStorage(
	files: QueueFileAPI,
	directory: string,
	log: (message: string) => void = () => {},
	recorderDirectory: string | null = null,
): QueueStorage {
	const indexPath = join(directory, "index.json");
	const tempPath = join(directory, "index.json.tmp");
	const corruptPath = join(directory, "index.corrupt.json");

	const readIndex = async (): Promise<QueueIndex> => {
		const text = await files.readText(indexPath);
		const parsed = text === null ? null : tryParse(text);
		if (parsed?.ok) return parsed.value;
		// Missing or torn index: a complete temp file is the newest state
		// (a crash between the temp write and the rename leaves exactly
		// that), so it wins over quarantining. Without one the rows come
		// from the recording files alone.
		const temp = await files.readText(tempPath);
		const fromTemp = temp === null ? null : tryParse(temp);
		if (parsed && !parsed.ok) {
			const outcome = fromTemp?.ok
				? "using the temp file"
				: "rebuilding it from the recording files";
			log(`queue index unreadable, ${outcome}: ${parsed.reason}`);
			// Best effort, and never a reason to fail the load: the rename
			// replaces an older corrupt copy, and when it fails the next save
			// writes over the unreadable index.
			await files.rename(indexPath, corruptPath).catch((error) => {
				log(`queue index not set aside: ${String(error)}`);
			});
		} else if (fromTemp && !fromTemp.ok) {
			log(
				`queue temp index unreadable, rebuilding the index from the recording files: ${fromTemp.reason}`,
			);
		}
		return fromTemp?.ok ? fromTemp.value : EMPTY_INDEX;
	};

	/** Moves `adoptRecorderFiles`' picks; the entries of the files moved. */
	const moveRecorderFiles = async (
		index: QueueIndex,
		queueEntries: readonly QueueDirectoryEntry[],
	): Promise<QueueDirectoryEntry[]> => {
		if (recorderDirectory === null) return [];
		let recorderEntries: QueueDirectoryEntry[];
		try {
			recorderEntries = await files.list(recorderDirectory);
		} catch (error) {
			log(`recorder directory unreadable, nothing moved: ${String(error)}`);
			return [];
		}
		const moved: QueueDirectoryEntry[] = [];
		for (const { from, to } of adoptRecorderFiles(
			index,
			queueEntries,
			recorderEntries,
		)) {
			try {
				await files.rename(
					join(recorderDirectory, from.name),
					join(directory, to),
				);
				moved.push({ ...from, name: to });
			} catch (error) {
				// Left where it is; the next load tries again.
				log(
					`recording ${from.name} not moved into the queue: ${String(error)}`,
				);
			}
		}
		if (moved.length > 0) {
			log(
				`moved ${moved.length} recording file(s) without a row from the recorder's directory`,
			);
		}
		return moved;
	};

	return {
		async load() {
			const loaded = await readIndex();
			let entries: QueueDirectoryEntry[];
			try {
				entries = await files.list(directory);
			} catch (error) {
				log(
					`queue directory unreadable, no recording files adopted: ${String(error)}`,
				);
				return loaded;
			}
			entries = [...entries, ...(await moveRecorderFiles(loaded, entries))];
			const adopted = adoptRecordingFiles(loaded, entries);
			const count = adopted.recordings.length - loaded.recordings.length;
			if (count > 0) {
				log(
					`queue index had no row for ${count} recording file(s), adopted them`,
				);
			}
			return adopted;
		},

		async save(index) {
			await files.writeText(tempPath, serializeQueueIndex(index));
			try {
				await files.rename(tempPath, indexPath);
			} catch (error) {
				await files.remove(tempPath).catch(() => {});
				throw error;
			}
		},
	};
}
