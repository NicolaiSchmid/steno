import type { SpeakerOptionsReply } from "./contract";
import {
	answerSpeakerOptions,
	applyScenario,
	createMockTransport,
	loadFixtureReplies,
	loadFixtureSnapshots,
	queryFromLocation,
} from "./mock-transport";
import type { BridgeTransport } from "./transport";

/**
 * The transport outside the app: the fixture-backed mock, bent by the page's
 * `?scenario=` and `?tab=`. `vite.config.ts` aliases `#bridge-fallback` here
 * for the dev server, `vite preview --mode screens`, Playwright and Vitest,
 * and to `fallback-none.ts` for the production bundle, so this module and
 * the fixtures it imports never reach the app. The speaker field's options
 * follow what is typed (`answerSpeakerOptions`).
 */
export function createFallbackTransport(): BridgeTransport {
	return createMockTransport({
		snapshots: async () =>
			applyScenario(await loadFixtureSnapshots(), queryFromLocation()),
		replies: async () => {
			const replies = await loadFixtureReplies();
			const recorded = replies["speakers.options"] as
				| SpeakerOptionsReply
				| undefined;
			return {
				...replies,
				"speakers.options": (params: unknown) =>
					answerSpeakerOptions(recorded, params),
			};
		},
	});
}
