import { Directory, File, Paths } from "expo-file-system";

import { RECORDER_DIRECTORY } from "@/features/recorder/recording-options";
import { type QueueFileAPI, UndecodableTextError } from "./queue-storage";

/**
 * `Documents/queue/`: the recordings and their index. Documents is backed up
 * and survives updates; the audio file protection is iOS's default.
 */
const queueDirectory = () => new Directory(Paths.document, "queue");

export function ensureQueueDirectory(): Directory {
	const directory = queueDirectory();
	if (!directory.exists) directory.create({ intermediates: true });
	return directory;
}

/** `Documents/ExpoAudio/`, where expo-audio writes a recording while it runs. */
export function recorderDirectory(): Directory {
	return new Directory(Paths.document, RECORDER_DIRECTORY);
}

/** A recording in the queue directory (created on demand); read `.exists`, `.size`, `.uri`. */
export function queuedFile(fileName: string): File {
	return new File(ensureQueueDirectory(), fileName);
}

/** `QueueFileAPI` over expo-file-system. Paths are `file://` URIs. */
export const expoQueueFiles: QueueFileAPI = {
	async readText(path) {
		const file = new File(path);
		if (!file.exists) return null;
		try {
			return await file.text();
		} catch (error) {
			// When the bytes read, the text did not decode: the file is
			// corrupt rather than unreadable. `bytes()` throws otherwise.
			await file.bytes();
			throw new UndecodableTextError(path, error);
		}
	},
	async writeText(path, text) {
		const file = new File(path);
		if (!file.exists) file.create({ intermediates: true, overwrite: true });
		file.write(text);
	},
	async rename(from, to) {
		await new File(from).move(new File(to), { overwrite: true });
	},
	async move(from, to) {
		// Without `overwrite` the move throws when `to` exists.
		await new File(from).move(new File(to));
	},
	async remove(path) {
		const file = new File(path);
		if (file.exists) file.delete();
	},
	async list(path) {
		const directory = new Directory(path);
		if (!directory.exists) return [];
		// The modification time, then the load time, stand in for a creation
		// time the file system does not report, so the row still gets a valid
		// `startedAt`.
		return directory
			.list()
			.filter((entry) => entry instanceof File)
			.map((file) => ({
				name: file.name,
				createdAt: file.creationTime ?? file.lastModified ?? Date.now(),
				size: file.size ?? 0,
			}));
	},
};
