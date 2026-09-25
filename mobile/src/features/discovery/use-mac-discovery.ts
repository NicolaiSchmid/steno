import {
	type MacService,
	type ResolvedMac,
	stenoLink,
} from "@modules/steno-link";
import { useEffect, useSyncExternalStore } from "react";
import { createMacRegistry, type RegistrySnapshot } from "./mac-registry";

/**
 * Bonjour browsing shared by the pairing sheet and the upload coordinator.
 * The native browser runs while at least one hook instance is mounted;
 * events land in one module-level registry.
 */
export const macRegistry = createMacRegistry();

let consumers = 0;
let subscriptions: { remove(): void }[] = [];

function startBrowsing() {
	const link = stenoLink();
	subscriptions = [
		link.addListener("serviceFound", (service) =>
			macRegistry.apply({ type: "found", service }),
		),
		link.addListener("serviceLost", (service) =>
			macRegistry.apply({ type: "lost", service }),
		),
		link.addListener("browserState", (state) =>
			macRegistry.apply({ type: "state", state }),
		),
	];
	link.startBrowsing();
}

function stopBrowsing() {
	for (const subscription of subscriptions) subscription.remove();
	subscriptions = [];
	stenoLink().stopBrowsing();
	macRegistry.apply({ type: "reset" });
}

/** Restart the browser after Wi-Fi came back or the app returned to the foreground. */
export function restartBrowsing() {
	if (consumers === 0) return;
	stopBrowsing();
	startBrowsing();
}

export function useMacDiscovery(enabled = true): RegistrySnapshot {
	useEffect(() => {
		if (!enabled) return;
		if (consumers === 0) startBrowsing();
		consumers += 1;
		return () => {
			consumers -= 1;
			if (consumers === 0) stopBrowsing();
		};
	}, [enabled]);

	return useSyncExternalStore(macRegistry.subscribe, macRegistry.snapshot);
}

export const MAC_WAIT_TIMEOUT_MS = 15_000;

/** Waits for the Mac with `macID` to be browsed, then resolves its host and port. */
export async function locateMac(
	macID: string,
	timeoutMs = MAC_WAIT_TIMEOUT_MS,
): Promise<{ service: MacService; resolved: ResolvedMac }> {
	const service = await macRegistry.waitForMac(macID, timeoutMs);
	const resolved = await stenoLink().resolve(service.name);
	return { service, resolved };
}
