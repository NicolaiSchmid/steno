import type { MeetingDetailSnapshot, SpeakerOptionsReply } from "./contract";
import {
	answerPlaying,
	answerSpeakerChange,
	answerSpeakerOptions,
	answerSpeakerSelect,
	answerSpeakerSeparate,
	answerTurnSpeaker,
	applyScenario,
	type CombineRecord,
	combineRecord,
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
 * person, Remove name, Restore, a paragraph's move, Separate again and
 * playing republish the meeting as the hosts do (`answerSpeakerSelect`,
 * `answerSpeakerChange`, `answerTurnSpeaker`, `answerSpeakerSeparate`,
 * `answerPlaying`).
 */
export function createFallbackTransport(): BridgeTransport {
	const query = queryFromLocation();
	const scenario = query.get("scenario") ?? "";
	let detail: MeetingDetailSnapshot | undefined;
	// The voices' Noes ("speakerID personID"), as the host keeps those
	// persons out of that voice's options.
	const rejected = new Set<string>();
	// The voices combined here, as they were, for Undo to put back.
	const combines = new Map<string, CombineRecord>();
	/** Republishes the meeting as `next` makes it; a throw is the reply. */
	const republish =
		(
			next: (
				now: MeetingDetailSnapshot,
				params: unknown,
			) => MeetingDetailSnapshot,
		) =>
		async (params: unknown) => {
			if (detail) {
				detail = next(detail, params);
				transport.emit("meeting.detail", detail);
			}
			return undefined;
		};
	const change = (method: string) =>
		republish((now, params) => answerSpeakerChange(now, method, params));
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
					const before = detail;
					detail = await answerSpeakerSelect(detail, params, scenario);
					const record = combineRecord(before, params);
					if (
						record &&
						!detail.speakers.some((row) => row.id === record.speaker.id)
					) {
						combines.set(record.speaker.id, record);
					}
					transport.emit("meeting.detail", detail);
					return undefined;
				},
				"speakers.separate": republish((now, params) => {
					const { speakerID } = params as { speakerID: string };
					const next = answerSpeakerSeparate(
						now,
						params,
						scenario,
						combines.get(speakerID),
					);
					combines.delete(speakerID);
					return next;
				}),
				"transcript.setTurnSpeaker": republish((now, params) =>
					answerTurnSpeaker(now, params, scenario),
				),
				"transcript.playTurn": republish((now, params) =>
					answerPlaying(now, {
						turn: (params as { segmentIDs: string[] }).segmentIDs[0] ?? "",
						fails: scenario === "speakers-play-failed",
					}),
				),
				"speakers.play": republish((now, params) =>
					answerPlaying(now, {
						voice: (params as { speakerID: string }).speakerID,
					}),
				),
				"speakers.stop": republish((now) => answerPlaying(now, null)),
			};
		},
	});
	transport.subscribe<MeetingDetailSnapshot>("meeting.detail", (snapshot) => {
		detail = snapshot;
	});
	return transport;
}
