import { act, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { RecordingSnapshot } from "@/bridge/contract";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { pushLevel } from "./panel-bar";
import {
	levelFraction,
	presentBubble,
	RecordingBubble,
} from "./recording-bubble";

const LIVE: RecordingSnapshot = {
	state: "recording",
	startedAt: new Date(Date.now() - 754_000).toISOString(),
	mode: "call",
	callApp: "Zoom",
	meetingID: "00000000-0000-0000-0000-000000000001",
	level: { mic: 0.42, system: 0.18 },
	deniedPermissions: [],
};

describe("presentBubble", () => {
	it("follows the recorder state as the Swift presentation does", () => {
		expect(presentBubble({ state: "starting", deniedPermissions: [] })).toEqual(
			{
				text: "Starting…",
				showsBars: false,
				showsStop: false,
				stopEnabled: false,
			},
		);
		expect(presentBubble(LIVE)).toEqual({
			showsBars: true,
			showsStop: true,
			stopEnabled: true,
		});
		expect(
			presentBubble({
				...LIVE,
				autoStop: {
					remainingSeconds: 42,
					totalSeconds: 60,
					reason: "Zoom closed",
				},
			}).autoStop,
		).toEqual({
			remainingSeconds: 42,
			totalSeconds: 60,
			reason: "Zoom closed",
		});
		expect(presentBubble({ state: "stopping", deniedPermissions: [] })).toEqual(
			{
				text: "Stopping…",
				showsBars: false,
				showsStop: true,
				stopEnabled: false,
			},
		);
		expect(presentBubble(undefined).showsStop).toBe(false);
	});

	it("takes the loudest lane and keeps five samples", () => {
		expect(levelFraction({ mic: 0.42, system: 0.18 })).toBe(0.42);
		expect(levelFraction(undefined)).toBe(0);
		expect(pushLevel([0, 0, 0, 0, 0], 0.5)).toEqual([0, 0, 0, 0, 0.5]);
		expect(pushLevel([0.1, 0.2, 0.3, 0.4, 0.5], 2)).toEqual([
			0.2, 0.3, 0.4, 0.5, 1,
		]);
	});
});

describe("RecordingBubble", () => {
	it("shows the elapsed time, the bars and the stop square while recording", async () => {
		const harness = await createBridgeHarness("", { recording: LIVE });
		renderWithBridge(<RecordingBubble />, harness);
		expect(await screen.findByTestId("bubble-elapsed")).toHaveTextContent(
			"12:34",
		);
		expect(screen.getByTestId("live-bars")).toBeInTheDocument();
		expect(
			screen.getByRole("button", { name: "Stop recording" }),
		).toBeEnabled();
		expect(screen.queryByTestId("bubble-auto-stop")).not.toBeInTheDocument();
	});

	it("stops through the bridge and opens the live meeting", async () => {
		const harness = await createBridgeHarness("", { recording: LIVE });
		renderWithBridge(<RecordingBubble />, harness);
		await screen.findByTestId("bubble-elapsed");
		await userEvent.click(screen.getByTestId("bubble-stop"));
		expect(callsTo(harness.transport, "recording.stop")).toHaveLength(1);
		await userEvent.click(screen.getByTestId("bubble-open"));
		expect(callsTo(harness.transport, "window.open")).toEqual([
			{
				method: "window.open",
				params: { window: "main", meetingID: LIVE.meetingID },
			},
		]);
	});

	it("shows the armed auto-stop with Keep recording", async () => {
		const harness = await createBridgeHarness("", {
			recording: {
				...LIVE,
				autoStop: {
					remainingSeconds: 42,
					totalSeconds: 60,
					reason: "Zoom closed.",
				},
			},
		});
		renderWithBridge(<RecordingBubble />, harness);
		expect(await screen.findByTestId("bubble-auto-stop")).toHaveTextContent(
			"Zoom closed: stops in 0:42",
		);
		expect(screen.getByTestId("countdown-hairline")).toBeInTheDocument();
		await userEvent.click(screen.getByTestId("bubble-keep-recording"));
		expect(callsTo(harness.transport, "recording.keepGoing")).toHaveLength(1);
	});

	it("says Starting… with no stop square, then Stopping… with it disabled", async () => {
		const harness = await createBridgeHarness("", {
			recording: { state: "starting", deniedPermissions: [] },
		});
		renderWithBridge(<RecordingBubble />, harness);
		expect(await screen.findByText("Starting…")).toBeInTheDocument();
		expect(screen.queryByTestId("bubble-stop")).not.toBeInTheDocument();
		act(() => {
			harness.transport.emit("recording", {
				state: "stopping",
				deniedPermissions: [],
			});
		});
		expect(await screen.findByText("Stopping…")).toBeInTheDocument();
		expect(screen.getByTestId("bubble-stop")).toBeDisabled();
	});

	it("sends page.ready so the host publishes the recorder", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<RecordingBubble />, harness);
		expect(callsTo(harness.transport, "page.ready")).toHaveLength(1);
	});
});
