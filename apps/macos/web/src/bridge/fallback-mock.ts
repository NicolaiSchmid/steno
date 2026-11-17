import type { MeetingDetailSnapshot, SpeakerOptionsReply } from "./contract";
import {
	answerSpeakerOptions,
	answerSpeakerSelect,
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
 * follow what is typed (`answerSpeakerOptions`), and a pick names the voice
 * and republishes the meeting as the hosts do (`answerSpeakerSelect`).
 */
export function createFallbackTransport(): BridgeTransport {
	const query = queryFromLocation();
	const scenario = query.get("scenario") ?? "";
	let detail: MeetingDetailSnapshot | undefined;
	const transport = createMockTransport({
		snapshots: async () => applyScenario(await loadFixtureSnapshots(), query),
		replies: async () => {
			const replies = await loadFixtureReplies();
			const recorded = replies["speakers.options"] as
				| SpeakerOptionsReply
				| undefined;
			return {
				...replies,
				"speakers.options": (params: unknown) =>
					answerSpeakerOptions(recorded, params, scenario),
				"speakers.select": async (params: unknown) => {
					if (!detail) {
						return undefined;
					}
					detail = await answerSpeakerSelect(detail, params, scenario);
					transport.emit("meeting.detail", detail);
					return undefined;
				},
			};
		},
	});
	transport.subscribe<MeetingDetailSnapshot>("meeting.detail", (snapshot) => {
		detail = snapshot;
	});
	return transport;
}
