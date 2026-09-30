import { act, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { MeetingDetailSnapshot } from "@/bridge/contract";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
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
