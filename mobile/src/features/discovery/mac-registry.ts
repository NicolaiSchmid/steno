import type { BrowserState, MacService } from "@modules/steno-link";

/**
 * Store of what the Bonjour browser currently sees, fed by the module's
 * `serviceFound` / `serviceLost` / `browserState` events. Snapshots are
 * immutable so `useSyncExternalStore` can compare them by identity.
 */
export type RegistrySnapshot = {
	/** Keyed by service instance name (the Mac's computer name). */
	services: readonly MacService[];
	browser: BrowserState | null;
};

export const EMPTY_REGISTRY: RegistrySnapshot = { services: [], browser: null };

/** The service advertising `macID` (case-insensitive), or null. */
export function findByMacID(
	services: readonly MacService[],
	macID: string,
): MacService | null {
	const wanted = macID.toLowerCase();
	return services.find((s) => s.macID?.toLowerCase() === wanted) ?? null;
}

export function createMacRegistry() {
	let current = EMPTY_REGISTRY;
	const listeners = new Set<() => void>();

	function set(next: RegistrySnapshot) {
		current = next;
		for (const listener of listeners) listener();
	}

	function subscribe(listener: () => void): () => void {
		listeners.add(listener);
		return () => {
			listeners.delete(listener);
		};
	}

	return {
		snapshot: () => current,
		subscribe,

		found(service: MacService) {
			const others = current.services.filter((s) => s.name !== service.name);
			set({ ...current, services: [...others, service] });
		},

		lost(service: MacService) {
			const services = current.services.filter((s) => s.name !== service.name);
			if (services.length === current.services.length) return;
			set({ ...current, services });
		},

		browserState(state: BrowserState) {
			set({ ...current, browser: state });
		},

		reset() {
			set(EMPTY_REGISTRY);
		},

		/** Resolves as soon as a service with `macID` is known, rejects on timeout. */
		waitForMac(macID: string, timeoutMs: number): Promise<MacService> {
			const known = findByMacID(current.services, macID);
			if (known) return Promise.resolve(known);
			return new Promise<MacService>((resolve, reject) => {
				const timer = setTimeout(() => {
					unsubscribe();
					reject(new Error("Mac not found on the local network"));
				}, timeoutMs);
				const unsubscribe = subscribe(() => {
					const found = findByMacID(current.services, macID);
					if (!found) return;
					clearTimeout(timer);
					unsubscribe();
					resolve(found);
				});
			});
		},
	};
}
