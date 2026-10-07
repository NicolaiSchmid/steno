import { stenoLink } from "@modules/steno-link/native";
import { File } from "expo-file-system";

import { queuedFile, recorderDirectory } from "@/features/queue/queue-files";
import { MIN_RECORDING_BYTES } from "./recording-options";
import type { RecoveryFiles } from "./recovery";

/**
 * `RecoveryFiles` over expo-file-system and the native hash.
 *
 * A row's `sourceUri` is absolute, but iOS moves the app's container to a
 * new path on an app update and on a restore from a backup, and the files
 * move with it. So `adopt` looks for the recorder's file by its name in the
 * current `Documents/ExpoAudio/`, as the queue storage's listing does.
 *
 * A queue file whose size cannot be read counts as present, with an unknown
 * size, and `adopt` replaces only a queue file it read as smaller than
 * `MIN_RECORDING_BYTES`, so an unreadable size never makes a full file look
 * empty.
 */
export const expoRecoveryFiles: RecoveryFiles = {
	// A throw here, from creating the queue directory or checking the file,
	// reads as an unknown size in `planRecovery`.
	size(fileName) {
		const file = queuedFile(fileName);
		return file.exists ? sizeOf(file) : 0;
	},
	async adopt(sourceUri, fileName) {
		const source = new File(recorderDirectory(), new File(sourceUri).name);
		const target = queuedFile(fileName);
		if (target.exists) {
			const size = sizeOf(target);
			if (size === null || size >= MIN_RECORDING_BYTES) {
				throw new Error(`${target.uri} may hold audio, not replaced`);
			}
		}
		await source.move(target, { overwrite: true });
	},
	sha256: (fileName) => stenoLink().sha256(queuedFile(fileName).uri),
};

/**
 * expo types `size` as a number, 0 when unreadable, but on iOS it is null
 * then (`try? file.size`), so both null and a throw read as unknown.
 */
function sizeOf(file: File): number | null {
	try {
		return file.size ?? null;
	} catch {
		return null;
	}
}
