import {
	findRecording,
	patchRecording,
	type QueueIndex,
	setState,
} from "@/features/queue/queue-index";
import { errorMessage } from "@/lib/error-message";
import { MIN_RECORDING_BYTES } from "./recording-options";

/**
 * A row still in `recording` at launch means the app died mid-recording.
 * While recording, expo-audio writes to its own directory, so the row carries
 * that `sourceUri`: recovery moves the file into the queue directory, hashes
 * it and queues it (the duration is estimated from the bit rate). A row with
 * no file of `MIN_RECORDING_BYTES` or more, in the queue or at its
 * `sourceUri`, is marked failed so the user sees why nothing arrived. A row
 * whose queue file exists but whose size cannot be read is left as it is, so
 * nothing replaces that file and a later launch tries again. The queue
 * storage adds rows in the same state for recording files the index did not
 * list (`adoptRecordingFiles`), so they are hashed and queued here too.
 *
 * Two phases so the async file work never races a recording that starts in
 * the meantime: `planRecovery` inspects a snapshot, `applyRecovery` patches
 * only rows that are still `recording` when the index is next written.
 */
export type RecoveryFiles = {
	/**
	 * Bytes of the queued file: 0 when missing, null when it exists but its
	 * size cannot be read.
	 */
	size(fileName: string): number | null;
	/**
	 * Moves the recorder's file, found by the name in `sourceUri`, to
	 * `Documents/queue/<fileName>`. It replaces only a file there read as
	 * below `MIN_RECORDING_BYTES`, which holds no meaningful audio, and throws
	 * when the recorder's file is missing or the queue file may hold audio.
	 */
	adopt(sourceUri: string, fileName: string): Promise<void>;
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

/** Shown on a row whose recording no file holds. */
export const INTERRUPTED_MESSAGE =
	"Recording was interrupted before it was saved";

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
		let byteCount = sizeOrUnknown(files, rec.fileName);
		if (
			byteCount !== null &&
			byteCount < MIN_RECORDING_BYTES &&
			rec.sourceUri
		) {
			try {
				await files.adopt(rec.sourceUri, rec.fileName);
				byteCount = sizeOrUnknown(files, rec.fileName);
			} catch {
				byteCount = 0;
			}
		}
		// The file is there but its size is unknown: hashing it would queue a
		// wrong byte count, and failing it would leave it behind.
		if (byteCount === null) continue;
		if (byteCount < MIN_RECORDING_BYTES) {
			patches.push({
				recordingID,
				kind: "failed",
				lastError: INTERRUPTED_MESSAGE,
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

/** A size that throws is unknown too. */
function sizeOrUnknown(files: RecoveryFiles, fileName: string): number | null {
	try {
		return files.size(fileName);
	} catch {
		return null;
	}
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
