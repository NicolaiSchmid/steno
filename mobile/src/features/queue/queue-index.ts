/**
 * The local queue: pure operations over the JSON index kept in
 * `Documents/queue/index.json` (plan decision 7). Every function returns a new
 * index and never touches the file system; `queue-storage.ts` persists it.
 * Illegal state transitions throw `QueueError` so a coordinator bug surfaces
 * in tests instead of corrupting the index.
 */

export type SyncState =
	| "recording"
	| "queued"
	| "uploading"
	| "delivered"
	| "failed"
	| "unpaired";

export type QueuedRecording = {
	recordingID: string;
	/** Relative to the queue directory. */
	fileName: string;
	/** ISO 8601 with fractional seconds. */
	startedAt: string;
	durationSeconds: number;
	byteCount: number;
	/** Whole-file SHA-256, standard base64; null until the recording stopped. */
	sha256: string | null;
	chunkSize: number;
	uploadedChunks: number[];
	state: SyncState;
	attempts: number;
	nextAttemptAt: string | null;
	lastError: string | null;
	meetingID: string | null;
};

export type QueueIndex = { version: 1; recordings: QueuedRecording[] };

export const EMPTY_INDEX: QueueIndex = { version: 1, recordings: [] };

export type NewRecording = Pick<
	QueuedRecording,
	| "recordingID"
	| "fileName"
	| "startedAt"
	| "durationSeconds"
	| "byteCount"
	| "sha256"
	| "chunkSize"
>;

export type Chunk = { index: number; offset: number; length: number };

export class QueueError extends Error {
	constructor(message: string) {
		super(message);
		this.name = "QueueError";
	}
}

/** Legal `state` transitions. Same-state is always allowed (a patch). */
const TRANSITIONS: Record<SyncState, readonly SyncState[]> = {
	recording: ["queued", "failed"],
	queued: ["uploading", "unpaired", "failed"],
	uploading: ["queued", "delivered", "failed", "unpaired"],
	failed: ["queued"],
	unpaired: ["queued"],
	delivered: [],
};

export function findRecording(
	index: QueueIndex,
	recordingID: string,
): QueuedRecording | null {
	return index.recordings.find((r) => r.recordingID === recordingID) ?? null;
}

function requireRecording(
	index: QueueIndex,
	recordingID: string,
): QueuedRecording {
	const found = findRecording(index, recordingID);
	if (!found) throw new QueueError(`unknown recording ${recordingID}`);
	return found;
}

function replace(
	index: QueueIndex,
	recordingID: string,
	next: QueuedRecording,
): QueueIndex {
	return {
		version: 1,
		recordings: index.recordings.map((r) =>
			r.recordingID === recordingID ? next : r,
		),
	};
}

export function addRecording(
	index: QueueIndex,
	rec: NewRecording,
	state: "recording" | "queued" = "queued",
): QueueIndex {
	if (findRecording(index, rec.recordingID)) {
		throw new QueueError(`duplicate recording ${rec.recordingID}`);
	}
	if (!Number.isInteger(rec.chunkSize) || rec.chunkSize <= 0) {
		throw new QueueError("chunkSize must be a positive integer");
	}
	const row: QueuedRecording = {
		...rec,
		uploadedChunks: [],
		state,
		attempts: 0,
		nextAttemptAt: null,
		lastError: null,
		meetingID: null,
	};
	return { version: 1, recordings: [...index.recordings, row] };
}

/** Fields the recorder fills in when it stops; never the sync bookkeeping. */
export function patchRecording(
	index: QueueIndex,
	recordingID: string,
	patch: Partial<
		Pick<
			QueuedRecording,
			"durationSeconds" | "byteCount" | "sha256" | "fileName" | "lastError"
		>
	>,
): QueueIndex {
	const current = requireRecording(index, recordingID);
	return replace(index, recordingID, { ...current, ...patch });
}

export function setState(
	index: QueueIndex,
	recordingID: string,
	state: SyncState,
	patch: Partial<QueuedRecording> = {},
): QueueIndex {
	const current = requireRecording(index, recordingID);
	if (current.state !== state && !TRANSITIONS[current.state].includes(state)) {
		throw new QueueError(
			`illegal transition ${current.state} -> ${state} for ${recordingID}`,
		);
	}
	return replace(index, recordingID, {
		...current,
		...patch,
		recordingID,
		state,
	});
}

export function markChunk(
	index: QueueIndex,
	recordingID: string,
	chunk: number,
): QueueIndex {
	const current = requireRecording(index, recordingID);
	if (!Number.isInteger(chunk) || chunk < 0) {
		throw new QueueError(`chunk index must be a non-negative integer`);
	}
	if (current.uploadedChunks.includes(chunk)) return index;
	return replace(index, recordingID, {
		...current,
		uploadedChunks: [...current.uploadedChunks, chunk].sort((a, b) => a - b),
	});
}

/** Replaces the chunk set with what the Mac reports as received. */
export function syncChunks(
	index: QueueIndex,
	recordingID: string,
	receivedChunks: number[],
): QueueIndex {
	const current = requireRecording(index, recordingID);
	const unique = [...new Set(receivedChunks)].sort((a, b) => a - b);
	return replace(index, recordingID, { ...current, uploadedChunks: unique });
}

/**
 * A transient failure: back to `queued`, one more attempt, next try at
 * `now + backoffMs`. `uploading` and `queued` both accept it.
 */
export function scheduleRetry(
	index: QueueIndex,
	recordingID: string,
	now: Date,
	backoffMs: number,
	error: string,
): QueueIndex {
	const current = requireRecording(index, recordingID);
	return setState(index, recordingID, "queued", {
		attempts: current.attempts + 1,
		nextAttemptAt: new Date(now.getTime() + backoffMs).toISOString(),
		lastError: error,
	});
}

/** Manual retry or a fresh pairing: eligible now, counters reset. */
export function resetForUpload(
	index: QueueIndex,
	recordingID: string,
): QueueIndex {
	return setState(index, recordingID, "queued", {
		attempts: 0,
		nextAttemptAt: null,
		lastError: null,
	});
}

export function removeRecording(
	index: QueueIndex,
	recordingID: string,
): QueueIndex {
	requireRecording(index, recordingID);
	return {
		version: 1,
		recordings: index.recordings.filter((r) => r.recordingID !== recordingID),
	};
}

export function chunkPlan(byteCount: number, chunkSize: number): Chunk[] {
	if (!Number.isInteger(chunkSize) || chunkSize <= 0) {
		throw new QueueError("chunkSize must be a positive integer");
	}
	if (!Number.isInteger(byteCount) || byteCount < 0) {
		throw new QueueError("byteCount must be a non-negative integer");
	}
	const chunks: Chunk[] = [];
	for (let offset = 0, i = 0; offset < byteCount; offset += chunkSize, i++) {
		chunks.push({
			index: i,
			offset,
			length: Math.min(chunkSize, byteCount - offset),
		});
	}
	return chunks;
}

export function isPending(rec: QueuedRecording): boolean {
	return rec.state === "queued" || rec.state === "uploading";
}

function isDue(rec: QueuedRecording, now: Date): boolean {
	return (
		rec.nextAttemptAt === null ||
		new Date(rec.nextAttemptAt).getTime() <= now.getTime()
	);
}

/**
 * The oldest pending recording whose backoff has elapsed, or null. Oldest
 * first so a long meeting never starves behind a newer short one.
 */
export function nextUploadable(
	index: QueueIndex,
	now: Date,
): QueuedRecording | null {
	const due = index.recordings
		.filter((r) => isPending(r) && isDue(r, now))
		.sort((a, b) => a.startedAt.localeCompare(b.startedAt));
	return due[0] ?? null;
}

/** Earliest future `nextAttemptAt` among pending recordings, or null. */
export function nextRetryAt(index: QueueIndex, now: Date): Date | null {
	let earliest: Date | null = null;
	for (const rec of index.recordings) {
		if (!isPending(rec) || rec.nextAttemptAt === null) continue;
		const at = new Date(rec.nextAttemptAt);
		if (at.getTime() <= now.getTime()) continue;
		if (!earliest || at < earliest) earliest = at;
	}
	return earliest;
}
