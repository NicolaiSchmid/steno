import {
	CHUNK_SIZE,
	MIN_RECORDING_BYTES,
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
 * untouched and removes the temp file.
 *
 * Every load also lists the directory and adds a row for each recording file
 * no row names, so an index that was lost, torn or written stale never
 * strands a recording on disk (`adoptRecordingFiles`). Before that it sends
 * the rows that failed before they were hashed, with their file still on
 * disk, back to crash recovery (`reopenUnhashedRows`), and moves in the
 * recordings the recorder's directory holds with no row to recover them, left
 * by a crash before the row was saved (`adoptRecorderFiles`).
 *
 * A load that cannot read the index persists nothing and leaves every file
 * where it was; `queue-store.ts` then refuses to save until a load succeeds.
 * An index whose bytes are not text is corrupt instead, and set aside.
 */
export type QueueFileAPI = {
	/**
	 * Null when the file does not exist. Throws `UndecodableTextError` when
	 * the file reads but its bytes are not text.
	 */
	readText(path: string): Promise<string | null>;
	/** Creates or truncates. */
	writeText(path: string, text: string): Promise<void>;
	/** Replaces `to` if it exists. */
	rename(from: string, to: string): Promise<void>;
	/** Throws when `to` exists, so a move never replaces a recording. */
	move(from: string, to: string): Promise<void>;
	/** No-op when missing. */
	remove(path: string): Promise<void>;
	/** The files directly in `directory`; none when it does not exist. */
	list(directory: string): Promise<QueueDirectoryEntry[]>;
};

/** A file that reads, but whose bytes do not decode as text. */
export class UndecodableTextError extends Error {
	constructor(path: string, cause: unknown) {
		super(`${path} is not text: ${String(cause)}`);
		this.name = "UndecodableTextError";
	}
}

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
 * by id or file name, oldest first; a file below `MIN_RECORDING_BYTES` holds
 * no audio and is left out. A row the index has keeps its state, so a file
 * left behind by a `delivered` row is not uploaded again. A new row is
 * `recording` with no size and no hash, the state crash recovery
 * (`planRecovery` in `src/features/recorder/recovery.ts`) picks up at launch:
 * it hashes the file and queues it. `startedAt` is the file's creation time,
 * which is when expo-audio opened it for the recording; the move into the
 * queue directory keeps it.
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
			return recordingID &&
				entry.size >= MIN_RECORDING_BYTES &&
				!known.has(recordingID) &&
				!known.has(entry.name)
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

function fileNameOf(uri: string): string {
	return uri.slice(uri.lastIndexOf("/") + 1);
}

/**
 * `index` with the rows that were never hashed back in `recording`, so crash
 * recovery hashes and queues them under their own id: a `failed` row whose
 * file holds audio in the queue (`queueEntries`) or, by its `sourceUri`, in
 * the recorder's directory (`recorderEntries`), and every `queued` one, which
 * only a Retry of an earlier app version made and the upload planner skips
 * (recovery marks it failed again when its file is gone). Without this a
 * failed row's file in the recorder's directory would become a second row
 * beside it. A recorder file that another row also names stays with that
 * row: an earlier app version wrote every recording of one run to the same
 * file. Attempts and the error start over.
 */
export function reopenUnhashedRows(
	index: QueueIndex,
	queueEntries: readonly QueueDirectoryEntry[],
	recorderEntries: readonly QueueDirectoryEntry[],
): QueueIndex {
	const withAudio = (entries: readonly QueueDirectoryEntry[]) =>
		new Set(
			entries.filter((e) => e.size >= MIN_RECORDING_BYTES).map((e) => e.name),
		);
	const inQueue = withAudio(queueEntries);
	const inRecorder = withAudio(recorderEntries);
	const sources = index.recordings.flatMap((r) =>
		r.sourceUri ? [fileNameOf(r.sourceUri)] : [],
	);
	const ownRecorderFile = ({ sourceUri }: QueuedRecording) => {
		if (!sourceUri) return false;
		const name = fileNameOf(sourceUri);
		return (
			inRecorder.has(name) && sources.filter((n) => n === name).length === 1
		);
	};
	const reopens = (r: QueuedRecording) =>
		r.sha256 === null &&
		(r.state === "queued" ||
			(r.state === "failed" &&
				(inQueue.has(r.fileName) || ownRecorderFile(r))));
	if (!index.recordings.some(reopens)) return index;
	return {
		version: 1,
		recordings: index.recordings.map((r) =>
			reopens(r)
				? {
						...r,
						state: "recording",
						attempts: 0,
						nextAttemptAt: null,
						lastError: null,
					}
				: r,
		),
	};
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
 * like an expo-audio recording, is below `MIN_RECORDING_BYTES` (no audio;
 * the recorder's file for the next recording starts as a header), or a
 * `recording` row names it as its `sourceUri` (`planRecovery` moves it under
 * that row). Every other one, a crash before its row was saved, becomes a new
 * queue file named after the UUID in the recorder's name, unless a row or a
 * queue file already has that name.
 */
export function adoptRecorderFiles(
	index: QueueIndex,
	queueEntries: readonly QueueDirectoryEntry[],
	recorderEntries: readonly QueueDirectoryEntry[],
): RecorderAdoption[] {
	const recovered = new Set(
		index.recordings.flatMap((r) =>
			r.state === "recording" && r.sourceUri ? [fileNameOf(r.sourceUri)] : [],
		),
	);
	const taken = new Set([
		...index.recordings.flatMap((r) => [r.recordingID, r.fileName]),
		...queueEntries.map((e) => e.name),
	]);
	return recorderEntries.flatMap((from) => {
		const id = recorderFileID(from.name);
		if (!id || from.size < MIN_RECORDING_BYTES || recovered.has(from.name)) {
			return [];
		}
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
	const unreadableTempPath = join(directory, "index.unreadable.json");

	/** Null when missing; bytes that are not text parse as corrupt. */
	const readParsed = async (path: string) => {
		try {
			const text = await files.readText(path);
			return text === null ? null : tryParse(text);
		} catch (error) {
			if (!(error instanceof UndecodableTextError)) throw error;
			return { ok: false as const, reason: String(error) };
		}
	};

	/**
	 * The temp file, parsed. One that cannot be read at all is set aside, so
	 * the next save, which writes the temp file, does not replace rows it may
	 * hold; best effort, like setting the index aside.
	 */
	const readTemp = async () => {
		try {
			return await readParsed(tempPath);
		} catch (error) {
			await files.rename(tempPath, unreadableTempPath).catch((renameError) => {
				log(`queue temp index not set aside: ${String(renameError)}`);
			});
			return { ok: false as const, reason: String(error) };
		}
	};

	const readIndex = async (): Promise<QueueIndex> => {
		// Throws when the index cannot be read: the load fails, nothing is saved.
		const parsed = await readParsed(indexPath);
		if (parsed?.ok) return parsed.value;
		// Missing or torn index: a complete temp file is the newest state
		// (a crash between the temp write and the rename leaves exactly
		// that), so it wins over setting aside. Without one the rows come
		// from the recording files alone.
		const fromTemp = await readTemp();
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

	/** The recorder's directory; none when there is none or it cannot be listed. */
	const listRecorderDirectory = async (): Promise<QueueDirectoryEntry[]> => {
		if (recorderDirectory === null) return [];
		try {
			return await files.list(recorderDirectory);
		} catch (error) {
			log(`recorder directory unreadable, nothing moved: ${String(error)}`);
			return [];
		}
	};

	/** Moves `adoptions` into the queue; the entries of the files moved. */
	const moveRecorderFiles = async (
		adoptions: readonly RecorderAdoption[],
	): Promise<QueueDirectoryEntry[]> => {
		if (recorderDirectory === null) return [];
		const moved: QueueDirectoryEntry[] = [];
		for (const { from, to } of adoptions) {
			try {
				await files.move(
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
			const recorderEntries = await listRecorderDirectory();
			const reopened = reopenUnhashedRows(loaded, entries, recorderEntries);
			const reopenedCount = reopened.recordings.filter(
				(r, i) => r !== loaded.recordings[i],
			).length;
			if (reopenedCount > 0) {
				log(
					`${reopenedCount} recording(s) never hashed go through crash recovery again`,
				);
			}
			const moved = await moveRecorderFiles(
				adoptRecorderFiles(reopened, entries, recorderEntries),
			);
			const adopted = adoptRecordingFiles(reopened, [...entries, ...moved]);
			const count = adopted.recordings.length - reopened.recordings.length;
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
