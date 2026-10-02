import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { MeetingDetailSnapshot } from "@/bridge/contract";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { SpeakersPopover } from "./speakers-popover";

const NICOLAI = "00000000-0000-0000-0000-000000000014";
const UNNAMED = "00000000-0000-0000-0000-000000000017";

async function speakers(): Promise<MeetingDetailSnapshot["speakers"]> {
	const detail = (await loadFixtureSnapshots())[
		"meeting.detail"
	] as MeetingDetailSnapshot;
	return detail.speakers;
}

describe("SpeakersPopover", () => {
	it("opens from the header row with one row per speaker", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<SpeakersPopover speakers={await speakers()} />, harness);
		const trigger = screen.getByTestId("speakers-trigger");
		expect(trigger).toHaveTextContent("Nicolai, Jérôme, Anna +1");
		expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

		await user.click(trigger);
		const popover = await screen.findByRole("dialog");
		expect(within(popover).getAllByRole("listitem")).toHaveLength(4);
		const nicolai = within(popover).getByTestId(`speaker-row-${NICOLAI}`);
		expect(nicolai).toHaveTextContent("Nicolai");
		expect(nicolai).toHaveTextContent("nicolai@example.com");
		expect(within(nicolai).queryByText("Who is this?")).toBeNull();
		const unnamed = within(popover).getByTestId(`speaker-row-${UNNAMED}`);
		expect(unnamed).toHaveTextContent("Speaker 4");
		expect(within(unnamed).getByText("Who is this?")).toBeInTheDocument();
		expect(within(unnamed).queryByTestId(`speaker-play-${UNNAMED}`)).toBeNull();
	});

	it("renames a confirmed speaker inline", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<SpeakersPopover speakers={await speakers()} />, harness);
		await user.click(screen.getByTestId("speakers-trigger"));
		await user.click(await screen.findByTestId(`speaker-picker-${NICOLAI}`));

		const field = await screen.findByTestId(`speaker-field-${NICOLAI}`);
		expect(field).toHaveFocus();
		await screen.findAllByRole("option");
		expect(callsTo(harness.transport, "speakers.options")).toEqual([
			{
				method: "speakers.options",
				params: { speakerID: NICOLAI, query: "" },
			},
		]);

		await user.click(screen.getByRole("option", { name: /^Nicolai/ }));
		expect(callsTo(harness.transport, "speakers.select")).toEqual([
			{
				method: "speakers.select",
				params: {
					speakerID: NICOLAI,
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
		expect(screen.getByRole("dialog")).toBeInTheDocument();
	});

	it("expands one picker at a time", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<SpeakersPopover speakers={await speakers()} />, harness);
		await user.click(screen.getByTestId("speakers-trigger"));
		await user.click(await screen.findByTestId(`speaker-picker-${UNNAMED}`));
		expect(screen.getByTestId(`speaker-field-${UNNAMED}`)).toBeInTheDocument();
		await user.click(screen.getByTestId(`speaker-picker-${NICOLAI}`));
		expect(screen.queryByTestId(`speaker-field-${UNNAMED}`)).toBeNull();
		expect(screen.getByTestId(`speaker-field-${NICOLAI}`)).toBeInTheDocument();
	});

	it("plays a speaker's clip from the row", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<SpeakersPopover speakers={await speakers()} />, harness);
		await user.click(screen.getByTestId("speakers-trigger"));
		await user.click(await screen.findByTestId(`speaker-play-${NICOLAI}`));
		expect(callsTo(harness.transport, "speakers.play")).toEqual([
			{ method: "speakers.play", params: { speakerID: NICOLAI } },
		]);
	});
});
