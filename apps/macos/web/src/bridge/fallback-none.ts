import type { BridgeTransport } from "./transport";

/**
 * The production bundle's `#bridge-fallback` (`vite.config.ts`): inside the
 * app the page is served from the steno-app scheme and a missing message
 * handler is a host bug, never papered over with fixtures. The mock
 * transport and the recorded fixtures stay out of the bundle;
 * `scripts/check-bundle.mjs` fails the build if they land in `dist/`.
 */
export function createFallbackTransport(): BridgeTransport {
	throw new Error("The steno message handler is not installed");
}
