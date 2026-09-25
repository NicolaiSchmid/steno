import {
	findRecording,
	patchRecording,
	type QueueIndex,
	setState,
} from "@/features/queue/queue-index";
import { errorMessage } from "@/lib/error-message";

/**
 * A row still in `recording` at launch means the app died mid-recording.
 * If the file made it into the queue directory, hash it and queue it (the
 * duration is estimated from the bit rate); otherwise mark it failed so the
 * user sees why nothing arrived.
 *
 * Two phases so the async file work never races a recording that starts in
 * the meantime: `planRecovery` inspects a snapshot, `applyRecovery` patches
 * only rows that are still `recording` when the index is next written.
 */
export type RecoveryFiles = {
	/** Bytes on disk, 0 when missing. */
	size(fileName: string): number;
	sha256(fileName: string): Promise<string>;
};

export type RecoveryPatch =
	| {
			recordingID: string;
			kind: "queued";
			byteCount: number;
			sha256: string;
			durationSeconds: number;
	  }
	| { recordingID: string; kind: "failed"; lastError: string };

/** Mono AAC at 64 kbps: bytes per second of audio. */
const ESTIMATED_BYTES_PER_SECOND = 64_000 / 8;

export async function planRecovery(
	index: QueueIndex,
	files: RecoveryFiles,
): Promise<RecoveryPatch[]> {
	const patches: RecoveryPatch[] = [];
	for (const rec of index.recordings) {
		if (rec.state !== "recording") continue;
		const { recordingID } = rec;
		let byteCount = 0;
		try {
			byteCount = files.size(rec.fileName);
		} catch {
			byteCount = 0;
		}
		if (byteCount === 0) {
			patches.push({
				recordingID,
				kind: "failed",
				lastError: "Recording was interrupted before it was saved",
			});
			continue;
		}
		try {
			const sha256 = await files.sha256(rec.fileName);
			patches.push({
				recordingID,
				kind: "queued",
				byteCount,
				sha256,
				durationSeconds:
					rec.durationSeconds > 0
						? rec.durationSeconds
						: Math.round(byteCount / ESTIMATED_BYTES_PER_SECOND),
			});
		} catch (error) {
			patches.push({
				recordingID,
				kind: "failed",
				lastError: errorMessage(error),
			});
		}
	}
	return patches;
}

export function applyRecovery(
	index: QueueIndex,
	patches: RecoveryPatch[],
): QueueIndex {
	let next = index;
	for (const patch of patches) {
		if (findRecording(next, patch.recordingID)?.state !== "recording") continue;
		if (patch.kind === "failed") {
			next = setState(next, patch.recordingID, "failed", {
				lastError: patch.lastError,
			});
		} else {
			next = patchRecording(next, patch.recordingID, {
				byteCount: patch.byteCount,
				sha256: patch.sha256,
				durationSeconds: patch.durationSeconds,
			});
			next = setState(next, patch.recordingID, "queued");
		}
	}
	return next;
}
