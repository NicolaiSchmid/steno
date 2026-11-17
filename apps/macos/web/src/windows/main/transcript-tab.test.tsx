import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { MeetingDetailSnapshot } from "@/bridge/contract";
import { BridgeProvider } from "@/bridge/hooks";
import {
	applyScenario,
	type FixtureMap,
	loadFixtureSnapshots,
} from "@/bridge/mock-transport";
import { BridgeError } from "@/bridge/transport";
import { ToastRegion, TooltipProvider } from "@/components/ui";
import { PlatformProvider, SWIFT_MAC } from "@/lib/platform";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { STALE_MOVE, TURN_SCOPE_LINE, type Turn } from "./speaker-names";
import { scopeLine } from "./speaker-picker";
import { TranscriptTab } from "./transcript-tab";

const UNNAMED = "00000000-0000-0000-0000-000000000017";
const ANNA = "00000000-0000-0000-0000-000000000016";
const NICOLAI = "00000000-0000-0000-0000-000000000014";
const JEROME = "00000000-0000-0000-0000-000000000015";
const ANNA_PERSON = "00000000-0000-0000-0000-00000000000C";

afterEach(() => {
	vi.restoreAllMocks();
});

/** The fixture's speakers, each leading a paragraph (`speakers-partly`). */
async function detail(): Promise<MeetingDetailSnapshot> {
	return applyScenario(
		await loadFixtureSnapshots(),
		new URLSearchParams("scenario=speakers-partly"),
	)["meeting.detail"] as MeetingDetailSnapshot;
}

describe("TranscriptTab", () => {
	it("names a voice from a turn's name, then gives focus back to it", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<TranscriptTab detail={await detail()} />, harness);

		const name = screen.getAllByTestId(`speaker-picker-${UNNAMED}`)[0];
		if (!name) throw new Error("no unnamed turn");
		await user.click(name);
		const panel = await screen.findByTestId(`speaker-panel-${UNNAMED}`);
		// The panel plays the paragraph, not the voice's sample.
		expect(
			within(panel).getByRole("button", { name: "Play this paragraph" }),
		).toHaveTextContent("Paragraph");
		// An unnamed voice opens on All, and the panel says what that changes.
		expect(
			within(panel).getByRole("radio", { name: "All of Speaker 4" }),
		).toBeChecked();
		expect(
			within(panel).getByTestId(`speaker-panel-scope-${UNNAMED}`),
		).toHaveTextContent("Changes Speaker 4’s only paragraph in this meeting.");
		const options = await within(panel).findAllByRole("option");
		expect(options.map((option) => option.textContent)).toEqual([
			"AAnnaSounds like · strong",
			"NNicolai · combines with Speaker 1",
			// The recorded reply's own Create row, for the recording's query.
			"Create “a”",
			// The voice's own action closes the list.
			"Not a person",
		]);
		expect(screen.getByTestId(`speaker-field-${UNNAMED}`)).toHaveFocus();
		// The scope is read under the voice, before the field.
		expect(
			within(panel)
				.getByTestId(`speaker-panel-scope-${UNNAMED}`)
				.compareDocumentPosition(
					screen.getByTestId(`speaker-field-${UNNAMED}`),
				) & Node.DOCUMENT_POSITION_FOLLOWING,
		).toBeTruthy();

		await user.click(screen.getByRole("option", { name: /^Anna/ }));
		expect(callsTo(harness.transport, "speakers.select")).toEqual([
			{
				method: "speakers.select",
				params: {
					speakerID: UNNAMED,
					option: {
						reason: "voice",
						strength: "strong",
						kind: "person",
						label: "Anna",
						personID: "00000000-0000-0000-0000-00000000000C",
					},
					combine: false,
				},
			},
		]);
		await vi.waitFor(() =>
			expect(screen.queryByTestId(`speaker-panel-${UNNAMED}`)).toBeNull(),
		);
		expect(name).toHaveFocus();
	});

	it("marks a suggested name with a ? and says why", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<TranscriptTab detail={await detail()} />, harness);
		const name = screen.getAllByTestId(`speaker-picker-${ANNA}`)[0];
		if (!name) throw new Error("no suggested turn");
		expect(name).toHaveTextContent("Anna?");
		expect(name).toHaveAccessibleName(
			"Anna, suggested. Sounds like Anna · strong. Click to confirm or change",
		);
		await user.click(name);
		// The "?" is the grey bubble of the pill and the avatars, decoration only.
		expect(
			screen.getAllByTestId(`speaker-suggested-${ANNA}`)[0],
		).toHaveAttribute("aria-hidden", "true");
		const panel = await screen.findByTestId(`speaker-panel-${ANNA}`);
		expect(panel).toHaveTextContent("Sounds like Anna");
		// A suggested voice counts as unnamed: All.
		expect(
			within(panel).getByRole("radio", { name: "All of Speaker 3" }),
		).toBeChecked();
		await user.click(within(panel).getByRole("button", { name: "Yes" }));
		expect(callsTo(harness.transport, "speakers.select")).toHaveLength(1);
	});

	it("keeps the names one tab stop, the arrows moving between them", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		const snapshot = await detail();
		renderWithBridge(<TranscriptTab detail={snapshot} />, harness);
		const names = within(
			screen.getByTestId("tab-content-transcript"),
		).getAllByRole("button");
		expect(names).toHaveLength(snapshot.transcript.length);
		expect(names.map((name) => name.getAttribute("tabindex"))).toEqual([
			"0",
			...names.slice(1).map(() => "-1"),
		]);
		await user.tab();
		expect(names[0]).toHaveFocus();
		await user.keyboard("{ArrowDown}");
		expect(names[1]).toHaveFocus();
		await user.keyboard("{End}");
		expect(names.at(-1)).toHaveFocus();
		await user.tab({ shift: true });
		await user.tab();
		expect(names.at(-1)).toHaveFocus();
	});

	it("leads each turn with the avatar, the name and the time, without a play", async () => {
		const harness = await createBridgeHarness();
		const snapshot = await detail();
		renderWithBridge(<TranscriptTab detail={snapshot} />, harness);
		const [first] = snapshot.transcript;
		if (!first) throw new Error("fixture has no turn");
		const turn = screen.getByTestId(`turn-${first.id}`);
		expect(turn).toHaveTextContent(
			new RegExp(
				`^${first.speakerName[0]}${first.speakerName}\\d{2}:\\d{2} – \\d{2}:\\d{2}`,
			),
		);
		expect(within(turn).getAllByRole("button")).toHaveLength(1);
		expect(screen.queryByTestId(/^speaker-play-/)).toBeNull();
		expect(screen.queryByTestId("copy-transcript")).toBeNull();
	});
	it("reads a voice that is not a person so, its text kept", async () => {
		const harness = await createBridgeHarness();
		const dismissed = applyScenario(
			await loadFixtureSnapshots(),
			new URLSearchParams("scenario=speakers-dismissed"),
		)["meeting.detail"] as MeetingDetailSnapshot;
		renderWithBridge(<TranscriptTab detail={dismissed} />, harness);
		const name = screen.getAllByTestId(`speaker-picker-${UNNAMED}`)[0];
		if (!name) throw new Error("no dismissed turn");
		expect(name).toHaveTextContent("Not a person");
		const turn = dismissed.transcript.find(
			(entry) => entry.speakerID === UNNAMED,
		);
		expect(turn?.text).toBeTruthy();
		expect(screen.getAllByText(turn?.text ?? "").length).toBeGreaterThan(0);
	});

	it("says No to a suggestion from the turn's panel", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<TranscriptTab detail={await detail()} />, harness);
		const name = screen.getAllByTestId(`speaker-picker-${ANNA}`)[0];
		if (!name) throw new Error("no suggested turn");
		await user.click(name);
		await user.click(await screen.findByTestId(`speaker-panel-no-${ANNA}`));
		expect(callsTo(harness.transport, "speakers.reject")).toEqual([
			{
				method: "speakers.reject",
				params: {
					speakerID: ANNA,
					personID: "00000000-0000-0000-0000-00000000000C",
				},
			},
		]);
		// The suggestion goes at once; the field offers everyone else.
		expect(screen.queryByTestId(`speaker-panel-yes-${ANNA}`)).toBeNull();
		const options = await screen.findAllByRole("option");
		expect(
			options.some((option) => option.textContent?.startsWith("AAnna")),
		).toBe(false);
	});
});

/** Renders the transcript of `scenario`, and lets the host republish it. */
async function transcript(
	scenario = "speakers-partly",
	replies: FixtureMap = {},
) {
	const user = userEvent.setup();
	const harness = await createBridgeHarness("", {}, replies);
	const snapshot = applyScenario(
		await loadFixtureSnapshots(),
		new URLSearchParams(`scenario=${scenario}`),
	)["meeting.detail"] as MeetingDetailSnapshot;
	const tree = (next: MeetingDetailSnapshot) => (
		<>
			<TranscriptTab detail={next} />
			<ToastRegion />
		</>
	);
	const { rerender } = renderWithBridge(tree(snapshot), harness);
	const republish = (next: MeetingDetailSnapshot) =>
		rerender(
			<PlatformProvider platform={SWIFT_MAC}>
				<TooltipProvider delay={0}>
					<BridgeProvider client={harness.client}>{tree(next)}</BridgeProvider>
				</TooltipProvider>
			</PlatformProvider>,
		);
	const turnOf = (speakerID: string | undefined): Turn => {
		const turn = snapshot.transcript.find(
			(entry) => entry.speakerID === speakerID,
		);
		if (!turn) throw new Error(`no turn of ${speakerID}`);
		return turn;
	};
	/** Opens the panel of `turn` and waits for its options. */
	const openTurn = async (turn: Turn) => {
		await user.click(
			within(screen.getByTestId(`turn-${turn.id}`)).getByRole("button"),
		);
		await screen.findAllByRole("option");
	};
	return { user, harness, snapshot, republish, turnOf, openTurn };
}

describe("a paragraph's panel", () => {
	it("opens on This paragraph for a confirmed voice, on All for an unnamed one, fresh each time", async () => {
		const { user, turnOf, openTurn } = await transcript();
		await openTurn(turnOf(JEROME));
		const paragraph = screen.getByRole("radio", { name: "This paragraph" });
		expect(paragraph).toBeChecked();
		expect(
			screen.getByTestId(`speaker-panel-scope-${JEROME}`),
		).toHaveTextContent(TURN_SCOPE_LINE);
		// The voice's own action is All's: not offered for one paragraph.
		expect(screen.queryByTestId("speaker-action-clear")).toBeNull();
		// The arrows move the choice, one tab stop.
		paragraph.focus();
		await user.keyboard("{ArrowRight}");
		const all = screen.getByRole("radio", {
			name: "All 2 paragraphs of Jérôme",
		});
		expect(all).toBeChecked();
		expect(all).toHaveFocus();
		expect(
			screen.getByTestId(`speaker-panel-scope-${JEROME}`),
		).toHaveTextContent(
			"Changes all 2 paragraphs of Jérôme (Speaker 2) in this meeting.",
		);
		expect(await screen.findByTestId("speaker-action-clear")).toBeVisible();
		// Not remembered: open again, This paragraph again.
		await user.keyboard("{Escape}");
		await vi.waitFor(() =>
			expect(screen.queryByTestId(`speaker-panel-${JEROME}`)).toBeNull(),
		);
		await openTurn(turnOf(JEROME));
		expect(screen.getByRole("radio", { name: "This paragraph" })).toBeChecked();
		await user.keyboard("{Escape}");
		await vi.waitFor(() =>
			expect(screen.queryByTestId(`speaker-panel-${JEROME}`)).toBeNull(),
		);
		await openTurn(turnOf(UNNAMED));
		expect(
			screen.getByRole("radio", { name: "All of Speaker 4" }),
		).toBeChecked();
	});

	it("moves this paragraph only, every segment it folds with the voice shown", async () => {
		const { user, harness, snapshot, turnOf, openTurn } = await transcript();
		const turn = turnOf(NICOLAI);
		expect(turn.segmentIDs).toHaveLength(2);
		await openTurn(turn);
		await user.keyboard("Ann");
		await vi.waitFor(() =>
			expect(screen.getByRole("option", { name: /Create/ })).toBeVisible(),
		);
		await user.keyboard("{Enter}");
		expect(callsTo(harness.transport, "transcript.setTurnSpeaker")).toEqual([
			{
				method: "transcript.setTurnSpeaker",
				params: {
					meetingID: snapshot.id,
					generation: snapshot.transcriptGeneration,
					segments: turn.segmentIDs.map((id) => ({ id, speakerID: NICOLAI })),
					option: {
						reason: "voice",
						strength: "strong",
						kind: "person",
						label: "Anna",
						personID: ANNA_PERSON,
					},
				},
			},
		]);
		expect(callsTo(harness.transport, "speakers.select")).toHaveLength(0);
		expect(await screen.findByTestId("toast")).toHaveTextContent(
			"This paragraph is now Anna’s.",
		);
		await vi.waitFor(() =>
			expect(screen.queryByTestId(`speaker-panel-${NICOLAI}`)).toBeNull(),
		);
	});

	it("says the transcript changed with Refresh, which takes it as it is now and keeps the text", async () => {
		vi.spyOn(console, "error").mockImplementation(() => {});
		const { user, harness, snapshot, republish, turnOf, openTurn } =
			await transcript("speakers-partly", {
				"transcript.setTurnSpeaker": (params: unknown) => {
					if (
						(params as { generation: number }).generation ===
						snapshot.transcriptGeneration
					) {
						throw new BridgeError(
							"transcript.setTurnSpeaker",
							"stale",
							"The transcript changed. Refresh to see it as it is now.",
						);
					}
					return undefined;
				},
			});
		await openTurn(turnOf(JEROME));
		await user.keyboard("Ann");
		await vi.waitFor(() =>
			expect(screen.getByRole("option", { name: /Create/ })).toBeVisible(),
		);
		await user.keyboard("{Enter}");
		const stale = await screen.findByTestId("speaker-panel-stale");
		expect(stale).toHaveTextContent(STALE_MOVE);
		expect(stale).toHaveAttribute("role", "alert");
		expect(screen.queryByTestId("speaker-panel-failure-retry")).toBeNull();
		expect(screen.getByRole("combobox")).toHaveValue("Ann");
		// The host republished: the meeting was processed again meanwhile.
		republish({
			...snapshot,
			transcriptGeneration: snapshot.transcriptGeneration + 1,
		});
		await user.click(within(stale).getByRole("button", { name: "Refresh" }));
		expect(screen.queryByTestId("speaker-panel-stale")).toBeNull();
		expect(screen.getByRole("combobox")).toHaveValue("Ann");
		await vi.waitFor(() =>
			expect(screen.getByRole("option", { name: /Create/ })).toBeVisible(),
		);
		await user.keyboard("{Enter}");
		await vi.waitFor(() =>
			expect(
				callsTo(harness.transport, "transcript.setTurnSpeaker").map(
					(call) => (call.params as { generation: number }).generation,
				),
			).toEqual([
				snapshot.transcriptGeneration,
				snapshot.transcriptGeneration + 1,
			]),
		);
	});

	it("keeps a failed move's text with the red line and Retry", async () => {
		vi.spyOn(console, "error").mockImplementation(() => {});
		const { user, harness, turnOf, openTurn } = await transcript(
			"speakers-partly",
			{ "transcript.setTurnSpeaker": new Error("database is locked") },
		);
		await openTurn(turnOf(JEROME));
		await user.keyboard("Ann");
		await vi.waitFor(() =>
			expect(screen.getByRole("option", { name: /Create/ })).toBeVisible(),
		);
		await user.keyboard("{Enter}");
		expect(
			await screen.findByTestId("speaker-panel-failure"),
		).toHaveTextContent("database is locked");
		expect(screen.getByRole("combobox")).toHaveValue("Ann");
		await user.click(screen.getByTestId("speaker-panel-failure-retry"));
		await vi.waitFor(() =>
			expect(
				callsTo(harness.transport, "transcript.setTurnSpeaker"),
			).toHaveLength(2),
		);
	});

	it("gives a turn without a voice the same name and This paragraph only", async () => {
		const { user, harness, turnOf, openTurn } =
			await transcript("speakers-no-voice");
		const turn = turnOf(undefined);
		const name = within(screen.getByTestId(`turn-${turn.id}`)).getByRole(
			"button",
		);
		expect(name).toHaveTextContent("Unknown");
		await openTurn(turn);
		const panel = screen.getByTestId(`speaker-panel-turn-${turn.id}`);
		expect(within(panel).queryByRole("radiogroup")).toBeNull();
		expect(panel).toHaveTextContent(TURN_SCOPE_LINE);
		expect(screen.queryByTestId(/^speaker-action-/)).toBeNull();
		// Nothing typed lists only the people named here; Anna is found by typing.
		expect(screen.queryByRole("option", { name: /^Anna/ })).toBeNull();
		await user.type(within(panel).getByRole("combobox"), "Ann");
		await user.click(await screen.findByRole("option", { name: /^Anna/ }));
		expect(
			callsTo(harness.transport, "transcript.setTurnSpeaker")[0]?.params,
		).toMatchObject({
			segments: turn.segmentIDs.map((id) => ({ id })),
			option: { label: "Anna", personID: ANNA_PERSON },
		});
	});

	it("plays and stops the paragraph's own audio", async () => {
		const { user, harness, snapshot, republish, turnOf, openTurn } =
			await transcript();
		const turn = turnOf(NICOLAI);
		await openTurn(turn);
		await user.click(
			screen.getByRole("button", { name: "Play this paragraph" }),
		);
		expect(callsTo(harness.transport, "transcript.playTurn")).toEqual([
			{
				method: "transcript.playTurn",
				params: { meetingID: snapshot.id, segmentIDs: turn.segmentIDs },
			},
		]);
		republish({
			...snapshot,
			transcript: snapshot.transcript.map((entry) =>
				entry.id === turn.id ? { ...entry, isPlaying: true } : entry,
			),
		});
		await user.click(
			screen.getByRole("button", { name: "Stop this paragraph" }),
		);
		expect(callsTo(harness.transport, "speakers.stop")).toHaveLength(1);
	});

	it("says in red when the paragraph's audio failed after the host replied", async () => {
		const { snapshot, republish, turnOf, openTurn } = await transcript();
		const turn = turnOf(NICOLAI);
		await openTurn(turn);
		expect(screen.queryByTestId("turn-play-failure")).toBeNull();
		republish({
			...snapshot,
			transcript: snapshot.transcript.map((entry) =>
				entry.id === turn.id ? { ...entry, playFailed: true } : entry,
			),
		});
		expect(screen.getByTestId("turn-play-failure")).toHaveTextContent(
			"The paragraph could not be played.",
		);
	});

	it("says the recording is gone where the paragraph's play would be", async () => {
		const { user, harness, turnOf, openTurn } = await transcript(
			"speakers-no-recording",
		);
		const turn = turnOf(NICOLAI);
		await openTurn(turn);
		const play = screen.getByTestId(`turn-play-${turn.id}`);
		expect(play).toBeDisabled();
		expect(play).toHaveAccessibleName("No recording");
		expect(screen.getByTestId(`turn-play-${turn.id}-none`)).toHaveTextContent(
			"No recording",
		);
		await user.click(play);
		expect(callsTo(harness.transport, "transcript.playTurn")).toHaveLength(0);
	});

	it("combines an All pick without asking, and Undo separates the voice", async () => {
		const { user, harness, turnOf, openTurn } = await transcript(
			"speakers-partly",
			{
				"speakers.select": (params: unknown) => {
					if (!(params as { combine: boolean }).combine) {
						throw new BridgeError(
							"speakers.select",
							"wouldCombine",
							"Nicolai already speaks as Speaker 1 in this meeting.",
						);
					}
					return undefined;
				},
			},
		);
		await openTurn(turnOf(UNNAMED));
		await user.click(screen.getByRole("option", { name: /^Nicolai/ }));
		const toast = await screen.findByTestId("toast");
		expect(toast).toHaveTextContent("Speaker 4 combined with Nicolai.");
		await user.click(within(toast).getByRole("button", { name: "Undo" }));
		await vi.waitFor(() =>
			expect(callsTo(harness.transport, "speakers.separate")).toEqual([
				{ method: "speakers.separate", params: { speakerID: UNNAMED } },
			]),
		);
	});
});

describe("scopeLine", () => {
	it("says the pick changes every paragraph of the voice, and whose", async () => {
		const { speakers } = await detail();
		const [nicolai, , anna] = speakers;
		if (!nicolai || !anna) throw new Error("fixture speakers");
		expect(scopeLine(anna, 3)).toBe(
			"Changes all 3 paragraphs of Speaker 3 in this meeting.",
		);
		expect(scopeLine(nicolai, 2)).toBe(
			"Changes all 2 paragraphs of Nicolai (Speaker 1) in this meeting.",
		);
		expect(scopeLine({ ...nicolai, clusterLabel: "Me" }, 4)).toBe(
			"Changes all 4 paragraphs of Nicolai (you) in this meeting.",
		);
		expect(scopeLine(anna, 1)).toBe(
			"Changes Speaker 3’s only paragraph in this meeting.",
		);
		expect(scopeLine(nicolai, 1)).toBe(
			"Changes Nicolai’s only paragraph (Speaker 1) in this meeting.",
		);
		expect(scopeLine({ ...nicolai, clusterLabel: "Me" }, 1)).toBe(
			"Changes your only paragraph in this meeting.",
		);
	});
});
