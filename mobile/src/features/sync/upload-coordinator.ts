import {
	chunkPlan,
	nextRetryAt,
	nextUploadable,
	type QueueIndex,
} from "@/features/queue/queue-index";

/**
 * The upload planner (plan P6): one pure decision per tick. The hook in
 * `use-upload-coordinator.ts` executes the action and feeds results back
 * into the queue index; nothing here performs I/O.
 *
 * One recording at a time, oldest first; announce before any chunk; at most
 * two chunks in flight; complete only when every chunk is uploaded and
 * nothing is in flight.
 */
export type MacState = { reachable: boolean; serviceName: string | null };

export type Action =
	| { kind: "idle" }
	| { kind: "announce"; recordingID: string }
	| { kind: "upload-chunk"; recordingID: string; chunk: number }
	| { kind: "complete"; recordingID: string }
	| { kind: "wait"; until: string };

export const MAX_CHUNKS_IN_FLIGHT = 2;

/** Task ids shared with the native background session. */
export const taskIDs = {
	announce: (recordingID: string) => `${recordingID}/announce`,
	chunk: (recordingID: string, chunk: number) => `${recordingID}/${chunk}`,
	complete: (recordingID: string) => `${recordingID}/complete`,
} as const;

export function parseChunkTaskID(
	taskID: string,
): { recordingID: string; chunk: number } | null {
	const slash = taskID.lastIndexOf("/");
	if (slash <= 0) return null;
	const chunk = Number(taskID.slice(slash + 1));
	if (!Number.isInteger(chunk) || chunk < 0) return null;
	return { recordingID: taskID.slice(0, slash), chunk };
}

export function planNext(
	index: QueueIndex,
	mac: MacState,
	inFlight: ReadonlySet<string>,
	now: Date,
): Action {
	if (!mac.reachable) return { kind: "idle" };

	const rec = nextUploadable(index, now);
	if (!rec) {
		const retryAt = nextRetryAt(index, now);
		return retryAt
			? { kind: "wait", until: retryAt.toISOString() }
			: { kind: "idle" };
	}
	// Not finalised yet (no hash); the recorder patches it in shortly.
	if (rec.sha256 === null) return { kind: "idle" };

	if (rec.state === "queued") {
		return inFlight.has(taskIDs.announce(rec.recordingID))
			? { kind: "idle" }
			: { kind: "announce", recordingID: rec.recordingID };
	}

	const plan = chunkPlan(rec.byteCount, rec.chunkSize);
	const uploaded = new Set(rec.uploadedChunks);
	let flying = 0;
	let next: number | null = null;
	for (const chunk of plan) {
		if (uploaded.has(chunk.index)) continue;
		if (inFlight.has(taskIDs.chunk(rec.recordingID, chunk.index))) {
			flying += 1;
		} else if (next === null) {
			next = chunk.index;
		}
	}
	if (next !== null && flying < MAX_CHUNKS_IN_FLIGHT) {
		return { kind: "upload-chunk", recordingID: rec.recordingID, chunk: next };
	}
	if (next === null && flying === 0) {
		return inFlight.has(taskIDs.complete(rec.recordingID))
			? { kind: "idle" }
			: { kind: "complete", recordingID: rec.recordingID };
	}
	return { kind: "idle" };
}

export const BACKOFF_BASE_MS = 5_000;
export const BACKOFF_CAP_MS = 5 * 60_000;

/**
 * 5 s doubling to a 5 min cap, jittered to [base/2, base] by the injected
 * RNG so retries from several phones do not align.
 */
export function backoffMs(attempt: number, random: () => number): number {
	const exponent = Math.max(0, Math.floor(attempt) - 1);
	const base = Math.min(BACKOFF_BASE_MS * 2 ** exponent, BACKOFF_CAP_MS);
	const jitter = Math.min(1, Math.max(0, random()));
	return Math.round(base * (0.5 + 0.5 * jitter));
}
