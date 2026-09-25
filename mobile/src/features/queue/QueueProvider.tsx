import {
	createContext,
	type ReactNode,
	useCallback,
	useContext,
	useEffect,
	useMemo,
	useRef,
	useState,
} from "react";

import { ensureQueueDirectory, expoQueueFiles } from "./queue-files";
import { EMPTY_INDEX, type QueueIndex } from "./queue-index";
import { createQueueStorage } from "./queue-storage";

/**
 * React state over the persisted queue index. `update` applies a pure
 * transform and persists the result; calls are serialised so two callers
 * (recorder and upload coordinator) never interleave a read-modify-write.
 */
export type QueueContextValue = {
	index: QueueIndex;
	/** False until the index was read from disk. */
	ready: boolean;
	update(transform: (index: QueueIndex) => QueueIndex): Promise<QueueIndex>;
};

const QueueContext = createContext<QueueContextValue | null>(null);

export function QueueProvider({ children }: { children: ReactNode }) {
	const [storage] = useState(() =>
		createQueueStorage(expoQueueFiles, ensureQueueDirectory().uri, (message) =>
			console.warn(`[queue] ${message}`),
		),
	);
	const [index, setIndex] = useState<QueueIndex>(EMPTY_INDEX);
	const [ready, setReady] = useState(false);
	const latest = useRef<QueueIndex>(EMPTY_INDEX);
	const chain = useRef<Promise<unknown>>(Promise.resolve());

	useEffect(() => {
		let cancelled = false;
		chain.current = chain.current.then(async () => {
			try {
				const loaded = await storage.load();
				if (cancelled) return;
				latest.current = loaded;
				setIndex(loaded);
			} catch (error) {
				console.warn("[queue] load failed", error);
			} finally {
				if (!cancelled) setReady(true);
			}
		});
		return () => {
			cancelled = true;
		};
	}, [storage]);

	const update = useCallback(
		(transform: (index: QueueIndex) => QueueIndex) => {
			const run = chain.current.then(async () => {
				const next = transform(latest.current);
				if (next === latest.current) return next;
				await storage.save(next);
				latest.current = next;
				setIndex(next);
				return next;
			});
			// Keep the chain alive after a failure so later updates still run.
			chain.current = run.catch(() => {});
			return run;
		},
		[storage],
	);

	const value = useMemo(
		() => ({ index, ready, update }),
		[index, ready, update],
	);
	return (
		<QueueContext.Provider value={value}>{children}</QueueContext.Provider>
	);
}

export function useQueue(): QueueContextValue {
	const value = useContext(QueueContext);
	if (!value) throw new Error("useQueue must be used inside QueueProvider");
	return value;
}
