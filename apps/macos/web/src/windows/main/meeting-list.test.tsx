import { act, fireEvent, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { MeetingsListSnapshot } from "@/bridge/contract";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { MeetingList, QUERY_DEBOUNCE_MS } from "./meeting-list";

const FIRST = "00000000-0000-0000-0000-000000000001";
const FAILED = "00000000-0000-0000-0000-000000000044";

async function fixtureList(): Promise<MeetingsListSnapshot> {
	return (await loadFixtureSnapshots())[
		"meetings.list"
	] as MeetingsListSnapshot;
}

describe("MeetingList", () => {
	afterEach(() => {
		vi.restoreAllMocks();
	});

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

	it("scrolls the selected row into view when the selection changes", async () => {
		const scrolled: Element[] = [];
		Element.prototype.scrollIntoView = vi.fn(function (this: Element) {
			scrolled.push(this);
		});
		const harness = await createBridgeHarness();
		renderWithBridge(<MeetingList />, harness);
		expect(scrolled).toEqual([screen.getByTestId(`meeting-${FIRST}`)]);
		const list = await fixtureList();
		act(() => {
			harness.transport.emit("meetings.list", { ...list, selection: FAILED });
		});
		expect(scrolled.at(-1)).toBe(screen.getByTestId(`meeting-${FAILED}`));
		expect(
			vi.mocked(Element.prototype.scrollIntoView),
		).toHaveBeenLastCalledWith({ block: "nearest" });
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

	it("deletes from the row's context menu and leaves the alert to the host", async () => {
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
			"meetings.delete",
		]);
		expect(callsTo(harness.transport, "ui.confirmDestructive")).toHaveLength(0);
		expect(callsTo(harness.transport, "meetings.delete")[0]?.params).toEqual({
			meetingID: FAILED,
		});
	});

	it("leaves the list alone when the host reports the alert was declined", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness(
			"",
			{},
			{ "meetings.delete": { confirmed: false } },
		);
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
		expect(screen.getByTestId(`meeting-${FAILED}`)).toBeInTheDocument();
		expect(harness.transport.calls.map((call) => call.method)).toEqual([
			"meetings.delete",
		]);
	});

	it("deletes the selected meeting with the Delete key", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<MeetingList />, harness);
		screen.getByTestId(`meeting-${FAILED}`).focus();
		await user.keyboard("{Delete}");
		await vi.waitFor(() =>
			expect(callsTo(harness.transport, "meetings.delete")).toEqual([
				{ method: "meetings.delete", params: { meetingID: FIRST } },
			]),
		);
		expect(callsTo(harness.transport, "ui.confirmDestructive")).toHaveLength(0);
	});

	it("does not treat Backspace in the search as a delete", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<MeetingList />, harness);
		await user.type(screen.getByTestId("search-meetings"), "a{Backspace}");
		expect(callsTo(harness.transport, "meetings.delete")).toHaveLength(0);
	});

	it("explains an empty list per filter", async () => {
		const harness = await createBridgeHarness("scenario=empty");
		renderWithBridge(<MeetingList />, harness);
		expect(screen.getByTestId("empty-meetings-title")).toHaveTextContent(
			"No meetings yet",
		);
		expect(screen.queryByTestId("clear-filters")).not.toBeInTheDocument();
	});

	it("offers to clear the filters when nothing matches", async () => {
		const user = userEvent.setup();
		const list = await fixtureList();
		const harness = await createBridgeHarness("", {
			"meetings.list": {
				...list,
				query: "zzz",
				tagFilter: "q4",
				groups: [],
			} satisfies MeetingsListSnapshot,
		});
		renderWithBridge(<MeetingList />, harness);
		expect(screen.getByTestId("empty-meetings-title")).toHaveTextContent(
			"No meetings match",
		);
		expect(screen.getByTestId("search-meetings")).toHaveValue("zzz");
		await user.click(screen.getByTestId("clear-filters"));
		expect(harness.transport.calls).toEqual([
			{ method: "meetings.setFilter", params: { filter: "all" } },
			{ method: "meetings.setTagFilter", params: {} },
			{ method: "meetings.setQuery", params: { query: "" } },
		]);
		expect(screen.getByTestId("search-meetings")).toHaveValue("");
	});
});
