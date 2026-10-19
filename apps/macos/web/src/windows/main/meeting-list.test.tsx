import { act, fireEvent, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
	MeetingsListSnapshot,
	RecordingSnapshot,
} from "@/bridge/contract";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { MeetingList, QUERY_DEBOUNCE_MS, rowPreview } from "./meeting-list";

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

	it("marks the host's selection and selects a clicked row", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<MeetingList />, harness);
		expect(screen.getByTestId(`meeting-${FIRST}`)).toHaveAttribute(
			"aria-current",
			"true",
		);
		expect(screen.getByTestId(`meeting-${FAILED}`)).not.toHaveAttribute(
			"aria-current",
		);
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
			expect(harness.transport.calls).toEqual([
				{ method: "meetings.delete", params: { meetingID: FAILED } },
			]),
		);
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

const THIRD = "00000000-0000-0000-0000-000000000003";
const PROCESSING = "00000000-0000-0000-0000-000000000002";

describe("MeetingList rows", () => {
	it("says to download the speech model for a meeting that waits for it", async () => {
		const list = await fixtureList();
		const row = list.groups[0]?.meetings[0];
		if (!row) {
			throw new Error("fixture has no meeting");
		}
		const queued = { ...row, state: "queued" as const, preview: undefined };
		expect(rowPreview(queued, undefined, false)).toBe("Waiting to process.");
		expect(
			rowPreview(
				queued,
				{
					meetingID: row.id,
					stage: "modelsMissing",
					title: "Download the speech models in Settings",
					fraction: 0,
				},
				false,
			),
		).toBe("Download the speech models in Settings.");
	});

	it("says what a meeting waits for when another app holds Steno's data", async () => {
		const list = await fixtureList();
		const row = list.groups[0]?.meetings[0];
		if (!row) {
			throw new Error("fixture has no meeting");
		}
		const waiting = {
			meetingID: row.id,
			stage: "storeBusy",
			title: "Waiting for another app to finish with Steno's data",
			fraction: 0,
		};
		// Queued, or still `processing` when the store kept the mark.
		for (const state of ["queued", "processing"] as const) {
			expect(
				rowPreview({ ...row, state, preview: undefined }, waiting, false),
			).toBe("Waiting for another app to finish with Steno's data.");
		}
	});

	it("flags a ready meeting without a summary, and only that one", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<MeetingList />, harness);
		expect(screen.getByTestId(`meeting-${THIRD}`)).toHaveTextContent(
			/^In person.*No summary/,
		);
		expect(screen.getByTestId(`meeting-${FAILED}`)).not.toHaveTextContent(
			"No summary",
		);
		expect(screen.getAllByText("No summary")).toHaveLength(1);
	});

	it("shows Failed before the time and the reason as the preview", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<MeetingList />, harness);
		const row = screen.getByTestId(`meeting-${FAILED}`);
		expect(row).toHaveTextContent(/^CallFailed\d{1,2}:\d{2}/);
		expect(row).toHaveTextContent("Transcription failed: model not installed");
	});

	it("shows Working while processing with the host's step as the preview", async () => {
		const harness = await createBridgeHarness("scenario=processing");
		renderWithBridge(<MeetingList />, harness);
		const row = screen.getByTestId(`meeting-${PROCESSING}`);
		expect(row).toHaveAttribute("aria-current", "true");
		expect(row).toHaveTextContent(/^CallWorking\d{1,2}:\d{2}/);
		expect(row).toHaveTextContent("Standup");
	});

	/** The fixture list with its first row still `recording`. */
	async function recordingList(): Promise<MeetingsListSnapshot> {
		const list = await fixtureList();
		const first = list.groups[0];
		if (!first) {
			throw new Error("fixture has no day group");
		}
		const [row, ...rest] = first.meetings;
		if (!row) {
			throw new Error("fixture has no meeting");
		}
		return {
			...list,
			groups: [
				{
					...first,
					meetings: [
						{ ...row, state: "recording", preview: undefined },
						...rest,
					],
				},
				...list.groups.slice(1),
			],
		};
	}

	// Around a start or a stop the list and the recording snapshot publish
	// apart: while the recorder is not idle, a `recording` row is still live.
	it.each([
		["recording", "Live", "Recording now."],
		["starting", "Live", "Recording now."],
		["stopping", "Live", "Recording now."],
		[
			"idle",
			"Not processed",
			"Not saved yet. Steno will process it the next time it starts.",
		],
	] as const)(
		"shows a recording row with the recorder %s as %s",
		async (state, badge, preview) => {
			const live = (await loadFixtureSnapshots())["recording.live"];
			const harness = await createBridgeHarness("", {
				"meetings.list": await recordingList(),
				...(state === "recording"
					? { recording: live }
					: state === "idle"
						? {}
						: {
								recording: {
									state,
									deniedPermissions: [],
								} satisfies RecordingSnapshot,
							}),
			});
			renderWithBridge(<MeetingList />, harness);
			const element = screen.getByTestId(`meeting-${FIRST}`);
			expect(element).toHaveTextContent(
				new RegExp(`^Call${badge}\\d{1,2}:\\d{2}`),
			);
			expect(element).toHaveTextContent(preview);
			expect(element).not.toHaveTextContent("No summary");
		},
	);
});
