import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { MeetingDetailSnapshot } from "@/bridge/contract";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { TranscriptTab, transcriptText } from "./transcript-tab";

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
			"AnnaSuggested, 87% match",
			"Nicolai",
			"Create “Add “Anna Berger””",
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
						kind: "person",
						label: "Nicolai",
						personID: "00000000-0000-0000-0000-00000000000A",
					},
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

	it("copies the transcript as one line per turn", async () => {
		const user = userEvent.setup();
		const writeText = vi.fn().mockResolvedValue(undefined);
		Object.defineProperty(navigator, "clipboard", {
			value: { writeText },
			configurable: true,
		});
		const harness = await createBridgeHarness();
		const snapshot = await detail();
		renderWithBridge(<TranscriptTab detail={snapshot} />, harness);
		await user.click(screen.getByTestId("copy-transcript"));
		expect(writeText).toHaveBeenCalledWith(transcriptText(snapshot.transcript));
		expect(await screen.findByText("Copied")).toBeInTheDocument();
	});
});
