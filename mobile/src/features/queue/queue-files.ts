import { Directory, File, Paths } from "expo-file-system";

import type { QueueFileAPI } from "./queue-storage";

/**
 * `Documents/queue/`: the recordings and their index. Documents is backed up
 * and survives updates; the audio file protection is iOS's default.
 */
export function queueDirectory(): Directory {
	return new Directory(Paths.document, "queue");
}

export function ensureQueueDirectory(): Directory {
	const directory = queueDirectory();
	if (!directory.exists) directory.create({ intermediates: true });
	return directory;
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

export function deleteQueuedFile(fileName: string): void {
	const file = new File(queueDirectory(), fileName);
	if (file.exists) file.delete();
}

export function queuedFileUri(fileName: string): string {
	return new File(queueDirectory(), fileName).uri;
}

export function queuedFileExists(fileName: string): boolean {
	return new File(queueDirectory(), fileName).exists;
}

/** Bytes on disk, 0 when missing. */
export function queuedFileSize(fileName: string): number {
	const file = new File(queueDirectory(), fileName);
	return file.exists ? file.size : 0;
}
