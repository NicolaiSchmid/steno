import {
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

function join(directory: string, name: string): string {
	return `${directory.replace(/\/+$/, "")}/${name}`;
}

export function createQueueStorage(
	files: QueueFileAPI,
	directory: string,
	log: (message: string) => void = () => {},
): QueueStorage {
	const indexPath = join(directory, "index.json");
	const tempPath = join(directory, "index.json.tmp");
	const corruptPath = join(directory, "index.corrupt.json");

	return {
		async load() {
			const text = await files.readText(indexPath);
			const parsed = text === null ? null : tryParse(text);
			if (parsed?.ok) return parsed.value;
			// Missing or torn index: a complete temp file is the newest state
			// (a crash between the temp write and the rename leaves exactly
			// that), so it wins over quarantining.
			const temp = await files.readText(tempPath);
			const fromTemp = temp === null ? null : tryParse(temp);
			if (parsed && !parsed.ok) {
				const outcome = fromTemp?.ok ? "using the temp file" : "starting empty";
				log(`queue index unreadable, ${outcome}: ${parsed.reason}`);
				await files.remove(corruptPath);
				await files.rename(indexPath, corruptPath).catch(() => {});
			} else if (fromTemp && !fromTemp.ok) {
				log(`queue temp index unreadable, starting empty: ${fromTemp.reason}`);
			}
			return fromTemp?.ok ? fromTemp.value : EMPTY_INDEX;
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
