import { act, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { AppSnapshot, MeetingDetailSnapshot } from "@/bridge/contract";
import { applyScenario, loadFixtureSnapshots } from "@/bridge/mock-transport";
import { ToastRegion } from "@/components/ui";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { MeetingDetail } from "./meeting-detail";
import { transcriptText } from "./transcript-tab";

const FIRST = "00000000-0000-0000-0000-000000000001";

async function fixtureDetail(): Promise<MeetingDetailSnapshot> {
	return (await loadFixtureSnapshots())[
		"meeting.detail"
	] as MeetingDetailSnapshot;
}

describe("MeetingDetail", () => {
	it("says open setup in one muted line, and a notice in its callout with OK", async () => {
		const user = userEvent.setup();
		const app = (await loadFixtureSnapshots()).app as AppSnapshot;
		const setup = await createBridgeHarness();
		const first = renderWithBridge(<MeetingDetail />, setup);
		expect(await screen.findByTestId("setup-line")).toHaveTextContent(
			/^Summaries are off\. Set up\s*Not now$/,
		);
		// The retention line lives in the footer only, beside its switch.
		expect(screen.getByTestId("meeting-meta")).not.toHaveTextContent(
			/Deletes|Recording/,
		);
		expect(screen.queryByTestId("setup-banner")).not.toBeInTheDocument();
		await user.click(screen.getByTestId("setup-summaries"));
		await vi.waitFor(() =>
			expect(callsTo(setup.transport, "window.open")).toEqual([
				{
					method: "window.open",
					params: { window: "settings", section: "summaries" },
				},
			]),
		);
		await user.click(screen.getByTestId("tab-transcript"));
		expect(screen.queryByTestId("setup-line")).not.toBeInTheDocument();
		first.unmount();
		const harness = await createBridgeHarness("", {
			app: {
				...app,
				setupBanner: {
					title: "Steno now transcribes with Parakeet v3",
					body: "The speech model you chose before is not part of this version.",
					offersSummaries: false,
					offersVault: false,
					isNotice: true,
				},
			} satisfies AppSnapshot,
		});
		renderWithBridge(<MeetingDetail />, harness);
		const ok = await screen.findByTestId("banner-not-now");
		expect(ok).toHaveTextContent("OK");
		expect(screen.getByTestId("setup-banner")).toBeInTheDocument();
		expect(screen.queryByTestId("setup-line")).not.toBeInTheDocument();
		expect(screen.queryByTestId("setup-summaries")).not.toBeInTheDocument();
		expect(screen.queryByTestId("choose-vault")).not.toBeInTheDocument();
		await user.click(ok);
		await vi.waitFor(() =>
			expect(callsTo(harness.transport, "setup.dismissBanner")).toHaveLength(1),
		);
	});

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

	it("processes a meeting again and holds the button until the host answers", async () => {
		const user = userEvent.setup();
		let answer: (value: { confirmed: boolean }) => void = () => {};
		const pending = new Promise<{ confirmed: boolean }>((resolve) => {
			answer = resolve;
		});
		const harness = await createBridgeHarness(
			"scenario=failed",
			{},
			{ "meeting.processAgain": pending },
		);
		renderWithBridge(
			<>
				<MeetingDetail />
				<ToastRegion />
			</>,
			harness,
		);
		const button = await screen.findByTestId("process-again");
		expect(button).toHaveTextContent("Process again");
		expect(button).toBeEnabled();
		await user.click(button);
		expect(callsTo(harness.transport, "meeting.processAgain")).toEqual([
			{ method: "meeting.processAgain", params: null },
		]);
		expect(button).toBeDisabled();
		await act(async () => {
			answer({ confirmed: true });
			await pending;
		});
		expect(button).toBeEnabled();
		expect(await screen.findByTestId("toast")).toHaveTextContent(
			"Processing the meeting again.",
		);
	});

	it("does not toast Process again when the host declines or refuses it", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness(
			"scenario=failed",
			{},
			{ "meeting.processAgain": Promise.resolve({ confirmed: false }) },
		);
		renderWithBridge(
			<>
				<MeetingDetail />
				<ToastRegion />
			</>,
			harness,
		);
		const button = await screen.findByTestId("process-again");
		await user.click(button);
		await vi.waitFor(() => expect(button).toBeEnabled());
		expect(callsTo(harness.transport, "meeting.processAgain")).toHaveLength(1);
		expect(screen.queryByTestId("toast")).not.toBeInTheDocument();
	});

	it("shows Process again only when the host offers it and disables it while busy", async () => {
		const failed = applyScenario(
			await loadFixtureSnapshots(),
			new URLSearchParams("scenario=failed"),
		)["meeting.detail"] as MeetingDetailSnapshot;
		const ready = await fixtureDetail();
		const harness = await createBridgeHarness("scenario=failed");
		renderWithBridge(<MeetingDetail />, harness);
		expect(await screen.findByTestId("process-again")).toBeEnabled();
		act(() => {
			harness.transport.emit("meeting.detail", {
				...failed,
				isBusy: true,
			} satisfies MeetingDetailSnapshot);
		});
		expect(screen.getByTestId("process-again")).toBeDisabled();
		// Not offered (the recording is gone, or the host is the Swift app):
		// the failure says so in red, with nothing to run.
		act(() => {
			harness.transport.emit("meeting.detail", {
				...failed,
				canProcessAgain: false,
			} satisfies MeetingDetailSnapshot);
		});
		expect(screen.getByTestId("processing-failed")).toBeInTheDocument();
		expect(screen.queryByTestId("process-again")).not.toBeInTheDocument();
		expect(
			screen.queryByTestId("failure-rerun-summary"),
		).not.toBeInTheDocument();
		// A plain ready meeting: not offered.
		act(() => {
			harness.transport.emit("meeting.detail", ready);
		});
		expect(ready.state).toBe("ready");
		expect(screen.queryByText("Process again")).not.toBeInTheDocument();
		// A ready meeting the host offers it for: the page reads only the
		// host's flag, not the state.
		act(() => {
			harness.transport.emit("meeting.detail", {
				...ready,
				canProcessAgain: true,
			} satisfies MeetingDetailSnapshot);
		});
		expect(screen.getByTestId("process-again")).toBeEnabled();
	});

	it("announces a failed transcription in red with Process again, once", async () => {
		const harness = await createBridgeHarness("scenario=failed");
		renderWithBridge(<MeetingDetail />, harness);
		const line = await screen.findByTestId("processing-failed");
		expect(line).toHaveAttribute("role", "alert");
		expect(line).toHaveAttribute("data-tone", "destructive");
		expect(line).toHaveTextContent(
			"Processing failed. Transcription failed: model not installed",
		);
		// One recovery: the line's Process again, none in the header.
		expect(screen.getAllByTestId("process-again")).toHaveLength(1);
		expect(within(line).getByTestId("process-again")).toBeEnabled();
		expect(
			screen.queryByTestId("failure-rerun-summary"),
		).not.toBeInTheDocument();
		// No transcript: no tabs, which would all be empty, but the personal
		// notes stay below the line, still editable.
		expect(screen.queryByRole("tablist")).not.toBeInTheDocument();
		const notes = screen.getByRole("region", { name: "Notes" });
		expect(within(notes).getByRole("textbox")).toBeEnabled();
	});

	it("offers Re-run summary when only the summary failed", async () => {
		const user = userEvent.setup();
		const snapshots = await loadFixtureSnapshots();
		const failed = applyScenario(
			snapshots,
			new URLSearchParams("scenario=failed"),
		)["meeting.detail"] as MeetingDetailSnapshot;
		const ready = await fixtureDetail();
		const harness = await createBridgeHarness("scenario=failed", {
			"meeting.detail": {
				...failed,
				transcript: ready.transcript,
				failureReason: "Summary failed: the server answered 500.",
				canRerunSummary: true,
			} satisfies MeetingDetailSnapshot,
		});
		renderWithBridge(<MeetingDetail />, harness);
		const line = await screen.findByTestId("processing-failed");
		await user.click(within(line).getByTestId("failure-rerun-summary"));
		expect(callsTo(harness.transport, "meeting.rerunSummary")).toHaveLength(1);
		expect(screen.queryByTestId("process-again")).not.toBeInTheDocument();
		// A transcript to read: the tabs stay, and the failure stays on the
		// summary; the transcript reads as usual.
		expect(screen.getByRole("tablist")).toBeInTheDocument();
		await user.click(screen.getByTestId("tab-transcript"));
		expect(screen.queryByTestId("processing-failed")).not.toBeInTheDocument();
		expect(screen.getByTestId("tab-content-transcript")).toHaveTextContent(
			ready.transcript[0]?.text ?? "",
		);
	});

	it("says a summary was skipped because the API key is not available yet", async () => {
		const withheld = (await loadFixtureSnapshots())[
			"meeting.detail.keyWithheld"
		] as MeetingDetailSnapshot;
		const harness = await createBridgeHarness("", {
			"meeting.detail": withheld,
		});
		renderWithBridge(<MeetingDetail />, harness);
		const status = await screen.findByTestId("summary-status");
		expect(status).toHaveTextContent("Summary skipped");
		expect(status).toHaveTextContent(
			"Steno can't use your API key yet, so this meeting has no summary.",
		);
		expect(
			within(status).getByTestId("summary-status-action"),
		).toHaveTextContent("Set up summaries");
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

	it.each([
		[true, "true"],
		[false, "false"],
	])(
		"keeps the switch on only when the host confirms (%s)",
		async (confirmed, checked) => {
			const user = userEvent.setup();
			const harness = await createBridgeHarness(
				"",
				{},
				{ "meeting.setKeepAudio": { confirmed } },
			);
			renderWithBridge(<MeetingDetail />, harness);
			const toggle = screen.getByTestId("keep-audio");
			expect(toggle).toHaveAttribute("aria-checked", "false");
			await user.click(toggle);
			expect(callsTo(harness.transport, "meeting.setKeepAudio")).toEqual([
				{ method: "meeting.setKeepAudio", params: { value: true } },
			]);
			await vi.waitFor(() =>
				expect(toggle).toHaveAttribute("aria-checked", checked),
			);
		},
	);

	it("says why an incomplete meeting keeps its recording and offers Process again and the delete", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=kept-incomplete");
		renderWithBridge(<MeetingDetail initialMenuOpen />, harness);
		expect(
			within(screen.getByTestId("retention-row")).getByText(
				"Recording kept because the speakers or the transcript may be incomplete. Process again, or delete the recording.",
			),
		).toBeInTheDocument();
		await user.click(await screen.findByTestId("delete-recording"));
		expect(
			callsTo(harness.transport, "meeting.deleteRecordingNow"),
		).toHaveLength(1);
		expect(screen.getByTestId("process-again")).toBeEnabled();
		await user.click(screen.getByTestId("process-again"));
		expect(callsTo(harness.transport, "meeting.processAgain")).toHaveLength(1);
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

	it("warns when parts of the recording were replaced by silence", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<MeetingDetail />, harness);
		expect(screen.queryByTestId("audio-warning")).not.toBeInTheDocument();
		const damaged = await createBridgeHarness("scenario=damaged-audio");
		renderWithBridge(<MeetingDetail />, damaged);
		expect(screen.getByTestId("audio-warning")).toHaveTextContent(
			"About 0.1 seconds of the phone recording could not be read and was replaced by silence.",
		);
	});

	it("offers Export again after a failed export, and Reveal once delivered", async () => {
		const user = userEvent.setup();
		const detail = await fixtureDetail();
		const harness = await createBridgeHarness("scenario=export-failed");
		renderWithBridge(<MeetingDetail />, harness);
		const status = screen.getByTestId("export-status");
		expect(status).toHaveTextContent("Failed");
		expect(status).toHaveAttribute("role", "alert");
		expect(status).toHaveAttribute("data-tone", "destructive");
		await user.click(screen.getByTestId("export-again"));
		await user.click(screen.getByTestId("export-meeting"));
		expect(callsTo(harness.transport, "meeting.reexport")).toHaveLength(2);
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

	it("toasts once an export the user asked for has run", async () => {
		const user = userEvent.setup();
		const detail = await fixtureDetail();
		const harness = await createBridgeHarness("scenario=export-failed");
		renderWithBridge(
			<>
				<MeetingDetail />
				<ToastRegion />
			</>,
			harness,
		);
		await user.click(screen.getByTestId("export-again"));
		const publish = (status: "pending" | "delivered") =>
			act(() => {
				harness.transport.emit("meeting.detail", {
					...detail,
					export: {
						status,
						message:
							status === "pending" ? "Exporting…" : "Obsidian · Exported 10:02",
						canReexport: true,
						canReveal: status === "delivered",
					},
				} satisfies MeetingDetailSnapshot);
			});
		publish("pending");
		expect(screen.queryByTestId("toast")).not.toBeInTheDocument();
		publish("delivered");
		expect(await screen.findByTestId("toast")).toHaveTextContent(
			"Exported.Obsidian · Exported 10:02",
		);
	});

	it("lets the template be changed from the actions menu", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("tab=summary");
		renderWithBridge(<MeetingDetail />, harness);
		await user.click(screen.getByTestId("meeting-actions"));
		expect(
			await screen.findByRole("menuitemradio", { name: "Meeting notes" }),
		).toHaveAttribute("aria-checked", "true");
		await user.click(screen.getByRole("menuitemradio", { name: "Standup" }));
		await vi.waitFor(() =>
			expect(callsTo(harness.transport, "meeting.setTemplate")).toEqual([
				{ method: "meeting.setTemplate", params: { templateID: "standup" } },
			]),
		);
	});

	it("copies the transcript from the actions menu, one line per turn", async () => {
		const user = userEvent.setup();
		const writeText = vi.fn().mockResolvedValue(undefined);
		Object.defineProperty(navigator, "clipboard", {
			value: { writeText },
			configurable: true,
		});
		const harness = await createBridgeHarness("tab=summary");
		renderWithBridge(<MeetingDetail />, harness);
		await user.click(screen.getByTestId("meeting-actions"));
		await user.click(await screen.findByTestId("copy-transcript"));
		expect(writeText).toHaveBeenCalledWith(
			transcriptText((await fixtureDetail()).transcript),
		);
	});

	it("keeps the meta line to source, date and length", async () => {
		const harness = await createBridgeHarness("tab=summary");
		renderWithBridge(<MeetingDetail />, harness);
		expect(screen.getByTestId("meeting-meta")).not.toHaveTextContent(
			/German|Deletes/,
		);
	});
});

describe("MeetingDetail header", () => {
	it("marks the header live, without a second Stop, while this meeting is being recorded", async () => {
		const harness = await createBridgeHarness("scenario=recording");
		renderWithBridge(<MeetingDetail />, harness);
		const live = screen.getByTestId("header-live");
		expect(live).toHaveTextContent(/^Recording\s*12:3\d$/);
		expect(live.closest("button")).toBeNull();
		// Live: no finished duration on the meta line.
		const meta = screen.getByTestId("meeting-meta");
		expect(meta.children).toHaveLength(2);
		expect(meta).not.toHaveTextContent("45:38");
		expect(
			screen.queryByRole("button", { name: /^Stop/ }),
		).not.toBeInTheDocument();
		// Nothing to export yet: no export line in the footer.
		expect(screen.getByTestId("meeting-footer")).toBeInTheDocument();
		expect(screen.queryByTestId("export-status")).not.toBeInTheDocument();
	});

	it("draws no footer while processing without a keep switch", async () => {
		const scenario = applyScenario(
			await loadFixtureSnapshots(),
			new URLSearchParams("scenario=processing"),
		)["meeting.detail"] as MeetingDetailSnapshot;
		const harness = await createBridgeHarness("scenario=processing", {
			"meeting.detail": {
				...scenario,
				retention: { ...scenario.retention, showsKeepToggle: false },
			} satisfies MeetingDetailSnapshot,
		});
		renderWithBridge(<MeetingDetail />, harness);
		expect(await screen.findByTestId("meeting-title")).toBeInTheDocument();
		expect(screen.queryByTestId("meeting-footer")).not.toBeInTheDocument();
	});

	it("keeps the live mark out of the header when another meeting records", async () => {
		const live = (await loadFixtureSnapshots())["recording.live"] as object;
		const harness = await createBridgeHarness("", {
			recording: { ...live, meetingID: "00000000-0000-0000-0000-000000000099" },
		});
		renderWithBridge(<MeetingDetail />, harness);
		expect(screen.queryByTestId("header-live")).not.toBeInTheDocument();
	});

	it("reads Stopping in the header while the recorder winds down", async () => {
		const live = (await loadFixtureSnapshots())["recording.live"] as object;
		const harness = await createBridgeHarness("", {
			recording: { ...live, state: "stopping" },
		});
		renderWithBridge(<MeetingDetail />, harness);
		expect(screen.getByTestId("header-live")).toHaveTextContent(/^Stopping…$/);
	});

	it("names speakers from one header button that reads how far naming got", async () => {
		const harness = await createBridgeHarness("scenario=speakers-none");
		renderWithBridge(<MeetingDetail />, harness);
		expect(screen.getByTestId("speakers-trigger")).toHaveTextContent(
			/^Name speakers$/,
		);
		expect(screen.queryByText(/Confirm speaker/)).not.toBeInTheDocument();

		const detail = applyScenario(
			await loadFixtureSnapshots(),
			new URLSearchParams("scenario=speakers-me"),
		)["meeting.detail"] as MeetingDetailSnapshot;
		act(() => harness.transport.emit("meeting.detail", detail));
		expect(screen.getByTestId("speakers-trigger")).toHaveTextContent(
			"1 needs a name",
		);

		act(() =>
			harness.transport.emit("meeting.detail", {
				...detail,
				speakers: detail.speakers.map((speaker) => ({
					...speaker,
					assignment: "confirmed",
					displayName:
						speaker.clusterLabel === "Me" ? speaker.displayName : "Jérôme Roux",
				})),
			} satisfies MeetingDetailSnapshot),
		);
		expect(screen.getByTestId("speakers-trigger")).toHaveTextContent(
			"Nicolai, Jérôme",
		);
	});

	it("says it is finding speakers while the progress topic reports diarizing", async () => {
		const harness = await createBridgeHarness("scenario=speakers-finding");
		renderWithBridge(<MeetingDetail />, harness);
		expect(screen.getByTestId("speakers-finding")).toHaveTextContent(
			"Finding speakers…",
		);
		expect(screen.queryByTestId("speakers-trigger")).toBeNull();
	});

	it("shows the empty state with nothing selected", async () => {
		const harness = await createBridgeHarness("scenario=empty");
		renderWithBridge(<MeetingDetail />, harness);
		expect(screen.queryByTestId("export-meeting")).not.toBeInTheDocument();
		expect(screen.getByTestId("empty-detail-title")).toHaveTextContent(
			"Select a meeting",
		);
	});
});
