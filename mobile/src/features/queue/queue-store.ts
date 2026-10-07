import { errorMessage } from "@/lib/error-message";
import { EMPTY_INDEX, QueueError, type QueueIndex } from "./queue-index";
import type { QueueStorage } from "./queue-storage";

/**
 * The queue index in memory over `QueueStorage`, without React so the load
 * and save rules are unit-tested. Loads and updates run one at a time in
 * call order, so two callers (recorder and upload coordinator) never
 * interleave a read-modify-write.
 *
 * Nothing is saved before a load succeeded: until then the index in memory
 * is not what the phone holds, and saving it would replace `index.json`
 * with fewer rows. A failed load is kept as `loadError`; the next `load()`
 * or `update()` tries again, and an update whose load still fails rejects
 * without writing.
 */
export type QueueSnapshot = {
	index: QueueIndex;
	/** True once a load succeeded; it stays true. */
	ready: boolean;
	/** Why the last load failed, while no load succeeded. */
	loadError: string | null;
};

export const QUEUE_NOT_LOADED_MESSAGE =
	"Steno could not read the list of recordings on this phone";

export type QueueStore = {
	snapshot(): QueueSnapshot;
	/** Calls `listener` after every change; returns the unsubscribe. */
	subscribe(listener: () => void): () => void;
	/** Loads the index unless a load already succeeded. */
	load(): Promise<void>;
	update(transform: (index: QueueIndex) => QueueIndex): Promise<QueueIndex>;
};

export function createQueueStore(
	storage: QueueStorage,
	log: (message: string) => void = () => {},
): QueueStore {
	let snapshot: QueueSnapshot = {
		index: EMPTY_INDEX,
		ready: false,
		loadError: null,
	};
	const listeners = new Set<() => void>();
	let chain: Promise<unknown> = Promise.resolve();

	const publish = (next: QueueSnapshot) => {
		snapshot = next;
		for (const listener of listeners) listener();
	};

	// Runs on the chain; the chain survives a rejection so later calls run.
	const enqueue = <T>(work: () => Promise<T>): Promise<T> => {
		const run = chain.then(work);
		chain = run.catch(() => {});
		return run;
	};

	const loadNow = async () => {
		if (snapshot.ready) return;
		try {
			const index = await storage.load();
			publish({ index, ready: true, loadError: null });
		} catch (error) {
			log(
				`load failed, nothing is saved until a load succeeds: ${errorMessage(error)}`,
			);
			publish({ ...snapshot, loadError: errorMessage(error) });
		}
	};

	return {
		snapshot: () => snapshot,
		subscribe(listener) {
			listeners.add(listener);
			return () => {
				listeners.delete(listener);
			};
		},
		load: () => enqueue(loadNow),
		update: (transform) =>
			enqueue(async () => {
				await loadNow();
				// The cause is in the log and in `loadError`; this message is
				// user-facing in case a caller shows it.
				if (!snapshot.ready) throw new QueueError(QUEUE_NOT_LOADED_MESSAGE);
				const next = transform(snapshot.index);
				if (next === snapshot.index) return next;
				await storage.save(next);
				publish({ ...snapshot, index: next });
				return next;
			}),
	};
}
