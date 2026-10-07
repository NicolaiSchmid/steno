import {
	createContext,
	type ReactNode,
	useContext,
	useEffect,
	useMemo,
	useState,
	useSyncExternalStore,
} from "react";
import { AppState } from "react-native";

import {
	ensureQueueDirectory,
	expoQueueFiles,
	recorderDirectory,
} from "./queue-files";
import type { QueueIndex } from "./queue-index";
import { createQueueStorage } from "./queue-storage";
import { createQueueStore } from "./queue-store";

/**
 * React state over the persisted queue index (`queue-store.ts`). `update`
 * applies a pure transform and persists the result; calls are serialised.
 * Nothing is saved until the index was read: a failed read shows as
 * `loadError` and is retried on `retryLoad`, on the next `update`, and
 * whenever the app comes to the foreground.
 */
export type QueueContextValue = {
	index: QueueIndex;
	/** False until the index was read from disk. */
	ready: boolean;
	/** Why the index could not be read, while it could not. */
	loadError: string | null;
	retryLoad(): void;
	update(transform: (index: QueueIndex) => QueueIndex): Promise<QueueIndex>;
};

const QueueContext = createContext<QueueContextValue | null>(null);

export function QueueProvider({ children }: { children: ReactNode }) {
	const [store] = useState(() => {
		const warn = (message: string) => console.warn(`[queue] ${message}`);
		return createQueueStore(
			createQueueStorage(
				expoQueueFiles,
				ensureQueueDirectory().uri,
				warn,
				recorderDirectory().uri,
			),
			warn,
		);
	});
	const snapshot = useSyncExternalStore(store.subscribe, store.snapshot);

	useEffect(() => {
		void store.load();
		const sub = AppState.addEventListener("change", (state) => {
			if (state === "active") void store.load();
		});
		return () => sub.remove();
	}, [store]);

	const value = useMemo(
		() => ({
			...snapshot,
			retryLoad: () => void store.load(),
			update: store.update,
		}),
		[snapshot, store],
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
