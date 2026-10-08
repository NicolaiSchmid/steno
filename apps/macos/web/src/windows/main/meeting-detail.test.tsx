import { act, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { MeetingDetailSnapshot } from "@/bridge/contract";
import { applyScenario, loadFixtureSnapshots } from "@/bridge/mock-transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { MeetingDetail } from "./meeting-detail";

const FIRST = "00000000-0000-0000-0000-000000000001";

async function fixtureDetail(): Promise<MeetingDetailSnapshot> {
	return (await loadFixtureSnapshots())[
		"meeting.detail"
	] as MeetingDetailSnapshot;
}

describe("MeetingDetail", () => {
	it("deletes from the actions menu and leaves the alert to the host", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<MeetingDetail initialMenuOpen />, harness);
		await user.click(await screen.findByTestId("delete-meeting"));
		await vi.waitFor(() =>
			expect(callsTo(harness.transport, "meetings.delete")).toEqual([
				{ method: "meetings.delete", params: { meetingID: FIRST } },
			]),
		);
		expect(callsTo(harness.transport, "ui.confirmDestructive")).toHaveLength(0);
	});

	it("offers to delete the recording now while its files exist", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<MeetingDetail initialMenuOpen />, harness);
		await user.click(await screen.findByTestId("delete-recording"));
		expect(
			callsTo(harness.transport, "meeting.deleteRecordingNow"),
		).toHaveLength(1);
		expect(callsTo(harness.transport, "ui.confirmDestructive")).toHaveLength(0);
	});

	it("hides Delete recording now once the files are gone", async () => {
		const detail = await fixtureDetail();
		const harness = await createBridgeHarness("", {
			"meeting.detail": {
				...detail,
				retention: { ...detail.retention, filesExist: false },
			} satisfies MeetingDetailSnapshot,
		});
		renderWithBridge(<MeetingDetail initialMenuOpen />, harness);
		await screen.findByTestId("delete-meeting");
		expect(screen.queryByTestId("delete-recording")).not.toBeInTheDocument();
	});

	it("processes a failed meeting again and holds the button until the host answers", async () => {
		const user = userEvent.setup();
		let answer: (value: undefined) => void = () => {};
		const pending = new Promise<undefined>((resolve) => {
			answer = resolve;
		});
		const harness = await createBridgeHarness(
			"scenario=failed",
			{},
			{ "meeting.processAgain": pending },
		);
		renderWithBridge(<MeetingDetail />, harness);
		const button = await screen.findByTestId("summary-process-again");
		expect(button).toHaveTextContent("Process again");
		expect(button).toBeEnabled();
		await user.click(button);
		expect(callsTo(harness.transport, "meeting.processAgain")).toEqual([
			{ method: "meeting.processAgain", params: null },
		]);
		expect(button).toBeDisabled();
		await act(async () => {
			answer(undefined);
			await pending;
		});
		expect(button).toBeEnabled();
	});

	it("disables Process again while busy and offers it only for a failed meeting with its recording", async () => {
		const failed = applyScenario(
			await loadFixtureSnapshots(),
			new URLSearchParams("scenario=failed"),
		)["meeting.detail"] as MeetingDetailSnapshot;
		const harness = await createBridgeHarness("scenario=failed");
		renderWithBridge(<MeetingDetail />, harness);
		expect(await screen.findByTestId("summary-process-again")).toBeEnabled();
		act(() => {
			harness.transport.emit("meeting.detail", {
				...failed,
				isBusy: true,
			} satisfies MeetingDetailSnapshot);
		});
		expect(screen.getByTestId("summary-process-again")).toBeDisabled();
		act(() => {
			harness.transport.emit("meeting.detail", {
				...failed,
				retention: { ...failed.retention, kind: "deleted", filesExist: false },
			} satisfies MeetingDetailSnapshot);
		});
		expect(screen.getByTestId("summary-try-again")).toBeInTheDocument();
		expect(
			screen.queryByTestId("summary-process-again"),
		).not.toBeInTheDocument();
		act(() => {
			harness.transport.emit("meeting.detail", {
				...failed,
				state: "ready",
			} satisfies MeetingDetailSnapshot);
		});
		expect(screen.queryByText("Process again")).not.toBeInTheDocument();
	});

	it("keeps Re-run summary for when the host allows it", async () => {
		const detail = await fixtureDetail();
		const harness = await createBridgeHarness("", {
			"meeting.detail": {
				...detail,
				canRerunSummary: false,
			} satisfies MeetingDetailSnapshot,
		});
		renderWithBridge(<MeetingDetail initialMenuOpen />, harness);
		expect(
			await screen.findByRole("menuitem", { name: "Re-run summary" }),
		).toHaveAttribute("aria-disabled", "true");
	});

	it("shows the keep switch and slides it back when the host declines", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness(
			"",
			{},
			{ "meeting.setKeepAudio": { confirmed: false } },
		);
		renderWithBridge(<MeetingDetail />, harness);
		const toggle = screen.getByTestId("keep-audio");
		expect(screen.getByTestId("retention-row")).toHaveTextContent(/^Deletes/);
		expect(toggle).toHaveAttribute("aria-checked", "false");
		await user.click(toggle);
		expect(callsTo(harness.transport, "meeting.setKeepAudio")).toEqual([
			{ method: "meeting.setKeepAudio", params: { value: true } },
		]);
		await vi.waitFor(() =>
			expect(toggle).toHaveAttribute("aria-checked", "false"),
		);
	});

	it("keeps the switch where the user put it when the host confirms", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<MeetingDetail />, harness);
		const toggle = screen.getByTestId("keep-audio");
		await user.click(toggle);
		await vi.waitFor(() =>
			expect(callsTo(harness.transport, "meeting.setKeepAudio")).toHaveLength(
				1,
			),
		);
		expect(toggle).toHaveAttribute("aria-checked", "true");
	});

	it("follows the host's keep flag and hides the switch when told to", async () => {
		const detail = await fixtureDetail();
		const harness = await createBridgeHarness();
		renderWithBridge(<MeetingDetail />, harness);
		act(() => {
			harness.transport.emit("meeting.detail", {
				...detail,
				retention: {
					kind: "keptForever",
					keepsAudio: true,
					showsKeepToggle: true,
					filesExist: true,
				},
			} satisfies MeetingDetailSnapshot);
		});
		expect(screen.getByTestId("keep-audio")).toHaveAttribute(
			"aria-checked",
			"true",
		);
		act(() => {
			harness.transport.emit("meeting.detail", {
				...detail,
				retention: { ...detail.retention, showsKeepToggle: false },
			} satisfies MeetingDetailSnapshot);
		});
		expect(screen.queryByTestId("keep-audio")).not.toBeInTheDocument();
	});

	it("reports where the export stands in the footer", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<MeetingDetail />, harness);
		expect(screen.getByTestId("export-status")).toHaveTextContent(
			"Not exported: no Obsidian vault is configured.",
		);
		expect(screen.queryByTestId("export-again")).not.toBeInTheDocument();
		expect(screen.queryByTestId("reveal-export")).not.toBeInTheDocument();
	});

	it("offers Export again after a failed export, and Reveal in Finder once delivered", async () => {
		const user = userEvent.setup();
		const detail = await fixtureDetail();
		const harness = await createBridgeHarness("scenario=export-failed");
		renderWithBridge(<MeetingDetail />, harness);
		expect(screen.getByTestId("export-status")).toHaveTextContent("Failed");
		await user.click(screen.getByTestId("export-again"));
		expect(callsTo(harness.transport, "meeting.reexport")).toHaveLength(1);
		act(() => {
			harness.transport.emit("meeting.detail", {
				...detail,
				export: {
					status: "delivered",
					message: "Obsidian · Exported 10:02",
					canReexport: true,
					canReveal: true,
				},
			} satisfies MeetingDetailSnapshot);
		});
		await user.click(screen.getByTestId("reveal-export"));
		expect(callsTo(harness.transport, "meeting.revealExport")).toHaveLength(1);
	});

	it("lets the template be changed beside the summary tab", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("tab=summary");
		renderWithBridge(<MeetingDetail />, harness);
		const trigger = screen.getByTestId("template-select");
		expect(trigger).toHaveTextContent("Meeting notes");
		await user.click(trigger);
		await user.click(await screen.findByRole("option", { name: "Standup" }));
		await vi.waitFor(() =>
			expect(callsTo(harness.transport, "meeting.setTemplate")).toEqual([
				{ method: "meeting.setTemplate", params: { templateID: "standup" } },
			]),
		);
	});

	it("hides the template picker away from the summary tab", async () => {
		const harness = await createBridgeHarness("tab=transcript");
		renderWithBridge(<MeetingDetail />, harness);
		expect(screen.queryByTestId("template-select")).not.toBeInTheDocument();
	});
});

describe("MeetingDetail header", () => {
	it("puts Meetings and the title in the breadcrumb with Export and the actions trigger", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<MeetingDetail />, harness);
		const header = screen.getByRole("banner");
		const crumbs = within(header).getAllByRole("listitem");
		expect(crumbs.map((crumb) => crumb.textContent)).toEqual([
			"Meetings",
			"Produktstrategie 90/10",
		]);
		expect(
			within(header).getByRole("listitem", { current: "page" }),
		).toHaveTextContent("Produktstrategie 90/10");
		expect(within(header).getByTestId("export-meeting")).toHaveTextContent(
			"Export",
		);
		expect(within(header).getByTestId("export-meeting")).toBeDisabled();
		expect(within(header).getByRole("button", { name: "More actions" })).toBe(
			screen.getByTestId("meeting-actions"),
		);
		expect(screen.queryByTestId("header-stop")).not.toBeInTheDocument();
	});

	it("enables Export once the host allows a re-export and sends it", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=export-failed");
		renderWithBridge(<MeetingDetail />, harness);
		await user.click(screen.getByTestId("export-meeting"));
		expect(callsTo(harness.transport, "meeting.reexport")).toHaveLength(1);
	});

	it("opens the actions menu from the trigger", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<MeetingDetail />, harness);
		await user.click(screen.getByRole("button", { name: "More actions" }));
		expect(
			await screen.findByRole("menuitem", { name: "Delete meeting…" }),
		).toBeInTheDocument();
	});

	it("shows Stop in the header only while this meeting is being recorded", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=recording");
		renderWithBridge(<MeetingDetail />, harness);
		const stop = screen.getByTestId("header-stop");
		expect(screen.getByRole("banner")).toContainElement(stop);
		expect(stop).toHaveTextContent(/^Stop\s*12:3\d$/);
		expect(stop.querySelector(".animate-status-pulse")).not.toBeNull();
		await user.click(stop);
		expect(callsTo(harness.transport, "recording.stop")).toHaveLength(1);
	});

	it("keeps Stop out of the header when another meeting records", async () => {
		const live = (await loadFixtureSnapshots())["recording.live"] as object;
		const harness = await createBridgeHarness("", {
			recording: { ...live, meetingID: "00000000-0000-0000-0000-000000000099" },
		});
		renderWithBridge(<MeetingDetail />, harness);
		expect(screen.queryByTestId("header-stop")).not.toBeInTheDocument();
	});

	it("disables Stop and reads Stopping while the recorder winds down", async () => {
		const live = (await loadFixtureSnapshots())["recording.live"] as object;
		const harness = await createBridgeHarness("", {
			recording: { ...live, state: "stopping" },
		});
		renderWithBridge(<MeetingDetail />, harness);
		const stop = screen.getByTestId("header-stop");
		expect(stop).toBeDisabled();
		expect(stop).toHaveTextContent(/^Stopping…$/);
		expect(stop.querySelector(".animate-status-pulse")).toBeNull();
	});

	it("shows the tags as badges beside the people and a Confirm speakers button", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("tab=summary");
		renderWithBridge(<MeetingDetail />, harness);
		const row = screen.getByTestId("speakers-row");
		expect(within(row).getByText("#strategie").tagName).toBe("SPAN");
		expect(within(row).getByText("#q4")).toHaveClass("border-input");
		const confirm = within(row).getByRole("button", {
			name: "Confirm speakers",
		});
		expect(confirm).toBe(screen.getByTestId("confirm-speaker"));
		await user.click(confirm);
		expect(callsTo(harness.transport, "meeting.setTab")).toEqual([
			{ method: "meeting.setTab", params: { tab: "transcript" } },
		]);
		expect(screen.getByTestId("tab-transcript")).toHaveAttribute(
			"aria-selected",
			"true",
		);
	});

	it("reads Confirm speaker for one and hides it with nothing to confirm", async () => {
		const detail = await fixtureDetail();
		const [a, b, c, d] = detail.speakers;
		if (!a || !b || !c || !d) {
			throw new Error("fixture has fewer than four speakers");
		}
		const harness = await createBridgeHarness("", {
			"meeting.detail": {
				...detail,
				speakers: [a, b, { ...c, assignment: "confirmed" }, d],
			} satisfies MeetingDetailSnapshot,
		});
		renderWithBridge(<MeetingDetail />, harness);
		expect(screen.getByTestId("confirm-speaker")).toHaveTextContent(
			"Confirm speaker",
		);
		act(() => {
			harness.transport.emit("meeting.detail", {
				...detail,
				speakers: detail.speakers.map((speaker) => ({
					...speaker,
					assignment: "confirmed",
				})),
			} satisfies MeetingDetailSnapshot);
		});
		expect(screen.queryByTestId("confirm-speaker")).not.toBeInTheDocument();
	});

	it("shows the Meetings crumb and the empty state with nothing selected", async () => {
		const harness = await createBridgeHarness("scenario=empty");
		renderWithBridge(<MeetingDetail />, harness);
		const header = screen.getByRole("banner");
		expect(
			within(header).getByRole("listitem", { current: "page" }),
		).toHaveTextContent("Meetings");
		expect(within(header).getAllByRole("listitem")).toHaveLength(1);
		expect(screen.queryByTestId("export-meeting")).not.toBeInTheDocument();
		expect(screen.getByTestId("empty-detail-title")).toHaveTextContent(
			"Select a meeting",
		);
	});
});
