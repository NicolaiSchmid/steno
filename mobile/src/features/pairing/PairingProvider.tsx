import {
	createContext,
	type ReactNode,
	useCallback,
	useContext,
	useEffect,
	useMemo,
	useState,
} from "react";

import { type Pairing, pairingStore } from "./pairing-store";

/**
 * The current pairing in React state, hydrated from the keychain once.
 * `replace` and `clear` persist first so a crash never leaves the UI ahead
 * of the store.
 */
export type PairingContextValue = {
	pairing: Pairing | null;
	/** False until the keychain was read. */
	ready: boolean;
	replace(pairing: Pairing): Promise<void>;
	clear(): Promise<void>;
};

const PairingContext = createContext<PairingContextValue | null>(null);

export function PairingProvider({ children }: { children: ReactNode }) {
	const [pairing, setPairing] = useState<Pairing | null>(null);
	const [ready, setReady] = useState(false);

	useEffect(() => {
		let cancelled = false;
		void pairingStore
			.load()
			.then((loaded) => {
				if (!cancelled) setPairing(loaded);
			})
			.catch((error) => console.warn("[pairing] load failed", error))
			.finally(() => {
				if (!cancelled) setReady(true);
			});
		return () => {
			cancelled = true;
		};
	}, []);

	const replace = useCallback(async (next: Pairing) => {
		await pairingStore.save(next.mac, next.token);
		setPairing(next);
	}, []);

	const clear = useCallback(async () => {
		await pairingStore.clear();
		setPairing(null);
	}, []);

	const value = useMemo(
		() => ({ pairing, ready, replace, clear }),
		[pairing, ready, replace, clear],
	);
	return (
		<PairingContext.Provider value={value}>{children}</PairingContext.Provider>
	);
}

export function usePairing(): PairingContextValue {
	const value = useContext(PairingContext);
	if (!value) throw new Error("usePairing must be used inside PairingProvider");
	return value;
}
