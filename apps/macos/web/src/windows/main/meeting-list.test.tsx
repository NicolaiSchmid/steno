import { fireEvent, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { MeetingList, QUERY_DEBOUNCE_MS } from "./meeting-list";

const FIRST = "00000000-0000-0000-0000-000000000001";
const FAILED = "00000000-0000-0000-0000-000000000044";

describe("MeetingList", () => {
	it("renders the day groups with the selected row raised", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<MeetingList />, harness);
		expect(screen.getByTestId(`meeting-${FIRST}`)).toHaveAttribute(
			"aria-current",
			"true",
		);
		expect(screen.getByTestId(`meeting-${FAILED}`)).not.toHaveAttribute(
			"aria-current",
		);
		expect(screen.getByTestId(`meeting-${FAILED}`)).toHaveTextContent(
			"Transcription failed: model not installed",
		);
	});

	it("records meetings.select when a row is clicked", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<MeetingList />, harness);
		await user.click(screen.getByTestId(`meeting-${FAILED}`));
		expect(callsTo(harness.transport, "meetings.select")).toEqual([
			{ method: "meetings.select", params: { meetingID: FAILED } },
		]);
	});

	it("moves the selection with the arrow keys", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<MeetingList />, harness);
		screen.getByTestId(`meeting-${FIRST}`).focus();
		await user.keyboard("{ArrowDown}");
		expect(callsTo(harness.transport, "meetings.select")[0]?.params).toEqual({
			meetingID: "00000000-0000-0000-0000-000000000003",
		});
	});

	it("debounces the search into one meetings.setQuery", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<MeetingList />, harness);
		await user.type(screen.getByTestId("search-meetings"), "inv");
		expect(callsTo(harness.transport, "meetings.setQuery")).toHaveLength(0);
		await vi.waitFor(
			() =>
				expect(callsTo(harness.transport, "meetings.setQuery")).toHaveLength(1),
			{ timeout: QUERY_DEBOUNCE_MS * 5, interval: 20 },
		);
		expect(callsTo(harness.transport, "meetings.setQuery")).toEqual([
			{ method: "meetings.setQuery", params: { query: "inv" } },
		]);
	});

	it("confirms with the host, then deletes, from the row's context menu", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<MeetingList />, harness);
		fireEvent.contextMenu(screen.getByTestId(`meeting-${FAILED}`), {
			clientX: 40,
			clientY: 40,
		});
		await user.click(
			await screen.findByRole("menuitem", { name: "Delete meeting…" }),
		);
		await vi.waitFor(() =>
			expect(callsTo(harness.transport, "meetings.delete")).toHaveLength(1),
		);
		expect(harness.transport.calls.map((call) => call.method)).toEqual([
			"ui.confirmDestructive",
			"meetings.delete",
		]);
		expect(callsTo(harness.transport, "meetings.delete")[0]?.params).toEqual({
			meetingID: FAILED,
		});
	});

	it("explains an empty list per filter", async () => {
		const harness = await createBridgeHarness("scenario=empty");
		renderWithBridge(<MeetingList />, harness);
		expect(screen.getByTestId("empty-meetings-title")).toHaveTextContent(
			"No meetings yet",
		);
	});
});
