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
import { AppState } from "react-native";

import { type Pairing, pairingStore } from "./pairing-store";

/**
 * The current pairing in React state, hydrated from the keychain at mount
 * and again on every return to the foreground while nothing is loaded: a
 * background relaunch on a locked phone can find the keychain unreadable,
 * and that must not look like "unpaired" for the rest of the process.
 * `replace` and `clear` persist first so a crash never leaves the UI ahead
 * of the store.
 */
export type PairingContextValue = {
	pairing: Pairing | null;
	/** False until the keychain was read once. */
	ready: boolean;
	replace(pairing: Pairing): Promise<void>;
	clear(): Promise<void>;
};

const PairingContext = createContext<PairingContextValue | null>(null);

export function PairingProvider({ children }: { children: ReactNode }) {
	const [pairing, setPairing] = useState<Pairing | null>(null);
	const [ready, setReady] = useState(false);
	const pairingRef = useRef<Pairing | null>(null);

	useEffect(() => {
		let cancelled = false;
		const load = () =>
			pairingStore
				.load()
				.then((loaded) => {
					if (cancelled || pairingRef.current) return;
					pairingRef.current = loaded;
					setPairing(loaded);
				})
				.catch((error) => console.warn("[pairing] load failed", error))
				.finally(() => {
					if (!cancelled) setReady(true);
				});
		void load();
		const sub = AppState.addEventListener("change", (state) => {
			if (state === "active" && pairingRef.current === null) void load();
		});
		return () => {
			cancelled = true;
			sub.remove();
		};
	}, []);

	const replace = useCallback(async (next: Pairing) => {
		await pairingStore.save(next);
		pairingRef.current = next;
		setPairing(next);
	}, []);

	const clear = useCallback(async () => {
		await pairingStore.clear();
		pairingRef.current = null;
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
