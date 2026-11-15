import type { MeetingDetailSnapshot, SpeakerOptionsReply } from "./contract";
import {
	answerSpeakerChange,
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
 * follow what is typed (`answerSpeakerOptions`), and a pick, a No, Not a
 * person, Remove name and Restore republish the meeting as the hosts do
 * (`answerSpeakerSelect`, `answerSpeakerChange`).
 */
export function createFallbackTransport(): BridgeTransport {
	const query = queryFromLocation();
	const scenario = query.get("scenario") ?? "";
	let detail: MeetingDetailSnapshot | undefined;
	// The voices' Noes ("speakerID personID"), as the host keeps those
	// persons out of that voice's options.
	const rejected = new Set<string>();
	const change = (method: string) => async (params: unknown) => {
		if (detail) {
			detail = answerSpeakerChange(detail, method, params);
			transport.emit("meeting.detail", detail);
		}
		return undefined;
	};
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
					answerSpeakerOptions(recorded, params, scenario, rejected),
				"speakers.reject": async (params: unknown) => {
					const { speakerID, personID } = params as {
						speakerID: string;
						personID: string;
					};
					rejected.add(`${speakerID} ${personID}`);
					return change("speakers.reject")(params);
				},
				"speakers.clear": change("speakers.clear"),
				"speakers.dismiss": change("speakers.dismiss"),
				"speakers.restore": change("speakers.restore"),
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
