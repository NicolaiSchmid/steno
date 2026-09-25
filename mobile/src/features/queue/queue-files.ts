import { Directory, File, Paths } from "expo-file-system";

import type { QueueFileAPI } from "./queue-storage";

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

/** A recording in the queue directory (created on demand); read `.exists`, `.size`, `.uri`. */
export function queuedFile(fileName: string): File {
	return new File(ensureQueueDirectory(), fileName);
}

/** `QueueFileAPI` over expo-file-system. Paths are `file://` URIs. */
export const expoQueueFiles: QueueFileAPI = {
	async readText(path) {
		const file = new File(path);
		if (!file.exists) return null;
		return file.text();
	},
	async writeText(path, text) {
		const file = new File(path);
		if (!file.exists) file.create({ intermediates: true, overwrite: true });
		file.write(text);
	},
	async rename(from, to) {
		await new File(from).move(new File(to), { overwrite: true });
	},
	async remove(path) {
		const file = new File(path);
		if (file.exists) file.delete();
	},
};
