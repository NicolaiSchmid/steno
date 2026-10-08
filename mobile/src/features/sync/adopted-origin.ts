import type { MacEndpoint } from "@modules/steno-link/native";
import { Paths } from "expo-file-system";

import { expoQueueFiles } from "@/features/queue/queue-files";
import type { QueueFileAPI } from "@/features/queue/queue-storage";

/**
 * The Mac's address the upload coordinator last chose, with the pin it was
 * chosen under, kept across a relaunch. Chunks the background session
 * carried over from the last process still go to that address, so the first
 * resolve after a launch compares against it and cancels them when it no
 * longer answers.
 *
 * A hint, not a record: it lives in `Documents/sync/adopted-origin.json`,
 * outside the queue index, and is written to a temp file renamed over it. A
 * file that is missing, cannot be read or does not parse reads as no earlier
 * address, as does a failed rename (expo removes the target before it
 * moves). Without it the first resolve adopts the new address and cancels
 * nothing, so a chunk carried over to a dead address waits for its own
 * failure (up to the session's 7-day resource timeout); no recording is
 * touched either way.
 */
export type AdoptedOrigin = {
	/** Waits for the writes already queued, so it sees the last address chosen. */
	read(): Promise<MacEndpoint | null>;
	/** Writes run one at a time, in call order. */
	write(endpoint: MacEndpoint): Promise<void>;
};

export function parseAdoptedOrigin(text: string): MacEndpoint | null {
	let parsed: unknown;
	try {
		parsed = JSON.parse(text);
	} catch {
		return null;
	}
	if (typeof parsed !== "object" || parsed === null) return null;
	const { origin, fingerprint } = parsed as Record<string, unknown>;
	return typeof origin === "string" &&
		origin.startsWith("https://") &&
		typeof fingerprint === "string" &&
		fingerprint !== ""
		? { origin, fingerprint }
		: null;
}

/**
 * `path` is a `file://` URI, computed at each read and write so that
 * importing this module does not touch `Paths.document`.
 */
export function createAdoptedOrigin(
	files: Pick<QueueFileAPI, "readText" | "writeText" | "rename">,
	path: () => string,
): AdoptedOrigin {
	// Never rejects: each write's failure goes to its own caller.
	let writes: Promise<void> = Promise.resolve();
	return {
		async read() {
			await writes;
			try {
				const text = await files.readText(path());
				return text === null ? null : parseAdoptedOrigin(text);
			} catch {
				return null;
			}
		},
		write({ origin, fingerprint }) {
			const written = writes.then(async () => {
				const target = path();
				const temp = `${target}.tmp`;
				await files.writeText(temp, JSON.stringify({ origin, fingerprint }));
				await files.rename(temp, target);
			});
			writes = written.catch(() => {});
			return written;
		},
	};
}

export const adoptedOrigin = createAdoptedOrigin(
	expoQueueFiles,
	() => `${Paths.document.uri.replace(/\/+$/, "")}/sync/adopted-origin.json`,
);
