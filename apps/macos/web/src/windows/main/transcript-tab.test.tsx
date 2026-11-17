import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { MeetingDetailSnapshot } from "@/bridge/contract";
import { applyScenario, loadFixtureSnapshots } from "@/bridge/mock-transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { scopeLine } from "./speaker-picker";
import { TranscriptTab } from "./transcript-tab";

const UNNAMED = "00000000-0000-0000-0000-000000000017";
const ANNA = "00000000-0000-0000-0000-000000000016";

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
		// The voice without a clip says so, on screen, where its play would be.
		expect(
			within(panel).getByRole("button", { name: "No sample" }),
		).toBeDisabled();
		expect(
			within(panel).getByTestId(`speaker-panel-play-${UNNAMED}-none`),
		).toHaveTextContent("No sample");
		// The pick changes the whole voice, and the panel says so.
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
		// Its play is the voice's sample, not this paragraph's audio.
		expect(
			within(panel).getByRole("button", {
				name: "Play the voice sample of Speaker 3",
			}),
		).toHaveTextContent("Voice sample");
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
