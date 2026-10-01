import { createFallbackTransport } from "#bridge-fallback";
import type { BridgeTransport } from "./transport";
import { createWebKitTransport, hasWebKitBridge } from "./webkit-transport";

export type { BridgeClient, BridgeClientOptions } from "./client";
export { ContractViolation, createBridgeClient } from "./client";
export * from "./contract";
export type { BridgeTransport, SnapshotHandler } from "./transport";
export { BridgeError, SnapshotHub } from "./transport";
export type { StenoHostApi } from "./webkit-transport";
export { createWebKitTransport, hasWebKitBridge } from "./webkit-transport";

/**
 * Picks the WebKit transport inside the app and the `#bridge-fallback`
 * module everywhere else: the fixture-backed mock on the Vite dev server,
 * `vite preview --mode screens`, Playwright and Vitest
 * (`fallback-mock.ts`), and a thrown error in the production bundle
 * (`fallback-none.ts`), which `vite.config.ts` picks by mode. The mock and
 * the fixtures are imported from `./mock-transport` directly by the tests
 * and never re-exported here, so nothing pulls them into the app.
 */
export function createBridge(): BridgeTransport {
	if (typeof window !== "undefined" && hasWebKitBridge(window)) {
		return createWebKitTransport(window);
	}
	// Inside the app the page is served from the steno-app scheme; a missing
	// handler there is a host bug and must not be papered over with fixtures.
	if (typeof location !== "undefined" && location.protocol === "steno-app:") {
		throw new Error("The steno message handler is not installed");
	}
	return createFallbackTransport();
}

let shared: BridgeTransport | undefined;

/** The one transport the page uses; created on first access. */
export function bridge(): BridgeTransport {
	shared ??= createBridge();
	return shared;
}
