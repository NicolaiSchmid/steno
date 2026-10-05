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
 * `replace`, `clear` and `clearIfCurrent` persist first so a crash never
 * leaves the UI ahead of the store, and run one at a time in call order.
 */
export type PairingContextValue = {
	pairing: Pairing | null;
	/** False until the keychain was read once. */
	ready: boolean;
	replace(pairing: Pairing): Promise<void>;
	clear(): Promise<void>;
	/**
	 * Runs `first`, then forgets the pairing, but only while it still holds
	 * `token`; resolves whether it did. `null` is a token nobody recorded and
	 * stands for the pairing loaded from the keychain at launch, so it
	 * matches nothing when none was. A 401 to a pairing since replaced or
	 * cleared leaves the current one alone.
	 */
	clearIfCurrent(
		token: string | null,
		first: () => Promise<unknown>,
	): Promise<boolean>;
};

const PairingContext = createContext<PairingContextValue | null>(null);

export function PairingProvider({ children }: { children: ReactNode }) {
	const [pairing, setPairing] = useState<Pairing | null>(null);
	const [ready, setReady] = useState(false);
	const pairingRef = useRef<Pairing | null>(null);
	// The token of the pairing this process started with, which `replace`
	// leaves alone: the one a request nobody recorded a token for was sent
	// with.
	const loadedTokenRef = useRef<string | null>(null);

	useEffect(() => {
		let cancelled = false;
		const load = () =>
			pairingStore
				.load()
				.then((loaded) => {
					if (cancelled || pairingRef.current) return;
					pairingRef.current = loaded;
					if (loaded) loadedTokenRef.current = loaded.token;
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

	const chain = useRef<Promise<unknown>>(Promise.resolve());
	const inOrder = useCallback(<T,>(change: () => Promise<T>): Promise<T> => {
		const run = chain.current.then(change);
		// Keep the chain alive after a failure so later changes still run.
		chain.current = run.catch(() => {});
		return run;
	}, []);

	const forget = useCallback(async () => {
		await pairingStore.clear();
		pairingRef.current = null;
		setPairing(null);
	}, []);

	const replace = useCallback(
		(next: Pairing) =>
			inOrder(async () => {
				await pairingStore.save(next);
				pairingRef.current = next;
				setPairing(next);
			}),
		[inOrder],
	);

	const clear = useCallback(() => inOrder(forget), [inOrder, forget]);

	const clearIfCurrent = useCallback(
		(token: string | null, first: () => Promise<unknown>) =>
			inOrder(async () => {
				const current = pairingRef.current;
				const expected = token ?? loadedTokenRef.current;
				if (!current || current.token !== expected) return false;
				await first();
				await forget();
				return true;
			}),
		[inOrder, forget],
	);

	const value = useMemo(
		() => ({ pairing, ready, replace, clear, clearIfCurrent }),
		[pairing, ready, replace, clear, clearIfCurrent],
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
