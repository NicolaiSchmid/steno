import type { BrowserState, MacService } from "@modules/steno-link";

/**
 * Pure store of what the Bonjour browser currently sees, fed by the module's
 * `serviceFound` / `serviceLost` / `browserState` events. Snapshots are
 * immutable so `useSyncExternalStore` can compare them by identity.
 */
export type RegistrySnapshot = {
	/** Keyed by service instance name (the Mac's computer name). */
	services: readonly MacService[];
	browser: BrowserState | null;
};

export type RegistryEvent =
	| { type: "found"; service: MacService }
	| { type: "lost"; service: MacService }
	| { type: "state"; state: BrowserState }
	| { type: "reset" };

export type MacRegistry = {
	snapshot(): RegistrySnapshot;
	apply(event: RegistryEvent): void;
	subscribe(listener: () => void): () => void;
	findByMacID(macID: string): MacService | null;
	/** Resolves as soon as a service with `macID` is known, rejects on timeout. */
	waitForMac(
		macID: string,
		timeoutMs: number,
		timers?: Pick<typeof globalThis, "setTimeout" | "clearTimeout">,
	): Promise<MacService>;
};

export const EMPTY_REGISTRY: RegistrySnapshot = { services: [], browser: null };

export function createMacRegistry(): MacRegistry {
	let current: RegistrySnapshot = EMPTY_REGISTRY;
	const listeners = new Set<() => void>();

	function set(next: RegistrySnapshot) {
		current = next;
		for (const listener of listeners) listener();
	}

	function findByMacID(macID: string): MacService | null {
		const wanted = macID.toLowerCase();
		return (
			current.services.find((s) => s.macID?.toLowerCase() === wanted) ?? null
		);
	}

	return {
		snapshot: () => current,

		apply(event) {
			switch (event.type) {
				case "found": {
					const others = current.services.filter(
						(s) => s.name !== event.service.name,
					);
					set({ ...current, services: [...others, event.service] });
					return;
				}
				case "lost": {
					const services = current.services.filter(
						(s) => s.name !== event.service.name,
					);
					if (services.length === current.services.length) return;
					set({ ...current, services });
					return;
				}
				case "state":
					set({ ...current, browser: event.state });
					return;
				case "reset":
					set(EMPTY_REGISTRY);
					return;
			}
		},

		subscribe(listener) {
			listeners.add(listener);
			return () => {
				listeners.delete(listener);
			};
		},

		findByMacID,

		waitForMac(macID, timeoutMs, timers = globalThis) {
			const known = findByMacID(macID);
			if (known) return Promise.resolve(known);
			return new Promise<MacService>((resolve, reject) => {
				const timer = timers.setTimeout(() => {
					unsubscribe();
					reject(new Error("Mac not found on the local network"));
				}, timeoutMs);
				const unsubscribe = this.subscribe(() => {
					const found = findByMacID(macID);
					if (!found) return;
					timers.clearTimeout(timer);
					unsubscribe();
					resolve(found);
				});
			});
		},
	};
}
