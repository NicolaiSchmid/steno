import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { NOTES_DEBOUNCE_MS, NotesTab } from "./notes-tab";

// Real timers: the debounce is a second, so the first test waits for it.
describe("NotesTab", () => {
	it("saves once, a second after the last keystroke", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(
			<NotesTab meetingID="00000000-0000-0000-0000-000000000001" notes="" />,
			harness,
		);
		await user.type(screen.getByTestId("scratchpad-editor"), "Call Anna");
		expect(callsTo(harness.transport, "meeting.saveNotes")).toHaveLength(0);
		await vi.waitFor(
			() =>
				expect(callsTo(harness.transport, "meeting.saveNotes")).toHaveLength(1),
			{ timeout: NOTES_DEBOUNCE_MS * 3, interval: 50 },
		);
		expect(callsTo(harness.transport, "meeting.saveNotes")).toEqual([
			{
				method: "meeting.saveNotes",
				params: {
					meetingID: "00000000-0000-0000-0000-000000000001",
					text: "Call Anna",
				},
			},
		]);
	});

	it("saves what is pending and flushes on unmount", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		const { unmount } = renderWithBridge(
			<NotesTab meetingID="00000000-0000-0000-0000-000000000001" notes="" />,
			harness,
		);
		await user.type(screen.getByTestId("scratchpad-editor"), "Draft");
		unmount();
		expect(harness.transport.calls.map((call) => call.method)).toEqual([
			"meeting.saveNotes",
			"meeting.flushNotes",
		]);
		expect(callsTo(harness.transport, "meeting.saveNotes")[0]?.params).toEqual({
			meetingID: "00000000-0000-0000-0000-000000000001",
			text: "Draft",
		});
	});

	it("shows the host's notes until the user types", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(
			<NotesTab
				meetingID="00000000-0000-0000-0000-000000000001"
				notes="From the host"
			/>,
			harness,
		);
		expect(screen.getByTestId("scratchpad-editor")).toHaveValue(
			"From the host",
		);
	});
});
