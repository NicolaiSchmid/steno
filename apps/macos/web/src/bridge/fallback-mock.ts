import {
	applyScenario,
	createMockTransport,
	loadFixtureSnapshots,
	queryFromLocation,
} from "./mock-transport";
import type { BridgeTransport } from "./transport";

/**
 * The transport outside the app: the fixture-backed mock, bent by the page's
 * `?scenario=` and `?tab=`. `vite.config.ts` aliases `#bridge-fallback` here
 * for the dev server, `vite preview --mode screens`, Playwright and Vitest,
 * and to `fallback-none.ts` for the production bundle, so this module and
 * the fixtures it imports never reach the app.
 */
export function createFallbackTransport(): BridgeTransport {
	return createMockTransport({
		snapshots: async () =>
			applyScenario(await loadFixtureSnapshots(), queryFromLocation()),
	});
}
