import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { MeetingDetailSnapshot } from "@/bridge/contract";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { TranscriptTab } from "./transcript-tab";

const UNNAMED = "00000000-0000-0000-0000-000000000017";
const NICOLAI = "00000000-0000-0000-0000-000000000014";

async function detail(): Promise<MeetingDetailSnapshot> {
	return (await loadFixtureSnapshots())[
		"meeting.detail"
	] as MeetingDetailSnapshot;
}

describe("TranscriptTab", () => {
	it("renames from a turn's name in a small popover", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<TranscriptTab detail={await detail()} />, harness);

		await user.click(screen.getByTestId(`speaker-picker-${UNNAMED}`));
		const options = await screen.findAllByRole("option");
		expect(options.map((option) => option.textContent)).toEqual([
			"AnnaSounds like",
			"NicolaiIn this meeting",
			"Create “a”",
		]);
		const field = screen.getByTestId(`speaker-field-${UNNAMED}`);
		expect(field).toHaveFocus();
		expect(callsTo(harness.transport, "speakers.options")).toEqual([
			{
				method: "speakers.options",
				params: { speakerID: UNNAMED, query: "" },
			},
		]);

		await user.click(screen.getByRole("option", { name: /^Nicolai/ }));
		expect(callsTo(harness.transport, "speakers.select")).toEqual([
			{
				method: "speakers.select",
				params: {
					speakerID: UNNAMED,
					option: {
						detail: "In this meeting",
						kind: "person",
						label: "Nicolai",
						personID: "00000000-0000-0000-0000-00000000000A",
					},
					combine: false,
				},
			},
		]);
		await vi.waitFor(() =>
			expect(screen.queryByRole("listbox")).not.toBeInTheDocument(),
		);
	});

	it("never offers Me's clip", async () => {
		const snapshot = await detail();
		const [first, ...rest] = snapshot.speakers;
		if (!first) {
			throw new Error("fixture has no speakers");
		}
		const harness = await createBridgeHarness();
		renderWithBridge(
			<TranscriptTab
				detail={{
					...snapshot,
					speakers: [{ ...first, clusterLabel: "Me", hasClip: true }, ...rest],
				}}
			/>,
			harness,
		);
		expect(screen.queryByTestId(`speaker-play-${NICOLAI}`)).toBeNull();
	});

	it("plays and stops a speaker's clip", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<TranscriptTab detail={await detail()} />, harness);
		await user.click(screen.getByTestId(`speaker-play-${NICOLAI}`));
		expect(callsTo(harness.transport, "speakers.play")).toEqual([
			{ method: "speakers.play", params: { speakerID: NICOLAI } },
		]);
	});

	it("leads each turn with the speaker in bold and the time beside it", async () => {
		const harness = await createBridgeHarness();
		const snapshot = await detail();
		renderWithBridge(<TranscriptTab detail={snapshot} />, harness);
		const [first] = snapshot.transcript;
		if (!first) throw new Error("fixture has no turn");
		const turn = screen.getByTestId(`turn-${first.id}`);
		expect(turn).toHaveTextContent(
			new RegExp(`^${first.speakerName}\\d{2}:\\d{2} – \\d{2}:\\d{2}`),
		);
		expect(screen.queryByTestId("copy-transcript")).toBeNull();
	});
});
