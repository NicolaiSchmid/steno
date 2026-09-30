import {
	createMockTransport,
	loadFixtureSnapshots,
	type MockTransport,
} from "./mock-transport";
import type { BridgeTransport } from "./transport";
import { createWebKitTransport, hasWebKitBridge } from "./webkit-transport";

export type { BridgeClient, BridgeClientOptions } from "./client";
export { ContractViolation, createBridgeClient } from "./client";
export * from "./contract";
export type {
	FixtureMap,
	MockTransport,
	MockTransportOptions,
	RecordedCall,
} from "./mock-transport";
export {
	createMockTransport,
	fixtureKey,
	isSnapshotKey,
	loadFixtureReplies,
	loadFixtureSnapshots,
	replyMethod,
} from "./mock-transport";
export type { BridgeTransport, SnapshotHandler } from "./transport";
export { BridgeError, SnapshotHub } from "./transport";
export type { StenoHostApi } from "./webkit-transport";
export { createWebKitTransport, hasWebKitBridge } from "./webkit-transport";

/**
 * Picks the WebKit transport inside the app and the fixture-backed mock
 * everywhere else (Vite dev server, `vite preview`, Playwright, tests).
 */
export function createBridge(): BridgeTransport | MockTransport {
	if (typeof window !== "undefined" && hasWebKitBridge(window)) {
		return createWebKitTransport(window);
	}
	// Inside the app the page is served from the steno-app scheme; a missing
	// handler there is a host bug and must not be papered over with fixtures.
	if (typeof location !== "undefined" && location.protocol === "steno-app:") {
		throw new Error("The steno message handler is not installed");
	}
	return createMockTransport({ snapshots: loadFixtureSnapshots });
}

let shared: BridgeTransport | MockTransport | undefined;

/** The one transport the page uses; created on first access. */
export function bridge(): BridgeTransport | MockTransport {
	shared ??= createBridge();
	return shared;
}
