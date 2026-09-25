import { type ResolvedMac, stenoLink } from "@modules/steno-link";
import { useEffect, useSyncExternalStore } from "react";
import { createMacRegistry, type RegistrySnapshot } from "./mac-registry";

/**
 * Bonjour browsing shared by the pairing sheet and the upload coordinator.
 * The native browser runs while at least one hook instance is mounted;
 * events land in one module-level registry.
 */
const macRegistry = createMacRegistry();

let consumers = 0;
let subscriptions: { remove(): void }[] = [];

function startBrowsing() {
	const link = stenoLink();
	subscriptions = [
		link.addListener("serviceFound", macRegistry.found),
		link.addListener("serviceLost", macRegistry.lost),
		link.addListener("browserState", macRegistry.browserState),
	];
	link.startBrowsing();
}

function stopBrowsing() {
	for (const subscription of subscriptions) subscription.remove();
	subscriptions = [];
	stenoLink().stopBrowsing();
	macRegistry.reset();
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

const MAC_WAIT_TIMEOUT_MS = 15_000;

/** Waits for the Mac with `macID` to be browsed, then resolves its host and port. */
export async function locateMac(
	macID: string,
	timeoutMs = MAC_WAIT_TIMEOUT_MS,
): Promise<ResolvedMac> {
	const service = await macRegistry.waitForMac(macID, timeoutMs);
	return stenoLink().resolve(service.name);
}
