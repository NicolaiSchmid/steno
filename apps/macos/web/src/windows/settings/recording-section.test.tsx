import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { RecordingSettingsSnapshot } from "@/bridge/contract";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { RecordingSection } from "./recording-section";

async function fixture(): Promise<RecordingSettingsSnapshot> {
	return (await loadFixtureSnapshots())[
		"settings.recording"
	] as RecordingSettingsSnapshot;
}

describe("RecordingSection", () => {
	it("shows the permissions, the folder with its usage and the retention rule", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<RecordingSection />, harness);
		expect(screen.getByTestId("permission-microphone")).toHaveTextContent(
			"Allowed",
		);
		expect(screen.getByTestId("permission-systemAudio")).toHaveTextContent(
			"Allowed",
		);
		expect(screen.getByTestId("audio-folder")).toHaveTextContent("audio");
		expect(screen.getByTestId("folder-usage")).toHaveTextContent(
			"Recordings use 734 MB",
		);
		expect(screen.getByTestId("retention-mode")).toHaveTextContent(
			"For 30 days",
		);
		expect(screen.getByTestId("retention-days")).toHaveValue(30);
		expect(screen.getByTestId("input-device")).toHaveTextContent(
			"System default",
		);
	});

	it("asks the host for the folder panel, the Finder and the device list", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<RecordingSection />, harness);
		await user.click(screen.getByTestId("choose-folder"));
		expect(
			callsTo(harness.transport, "settings.recording.chooseFolder"),
		).toHaveLength(1);
		await user.click(screen.getByTestId("reveal-folder"));
		expect(
			callsTo(harness.transport, "settings.recording.revealFolder"),
		).toHaveLength(1);
		await user.click(screen.getByTestId("refresh-devices"));
		expect(
			callsTo(harness.transport, "settings.recording.refreshDevices"),
		).toHaveLength(1);
	});

	it("saves the days when the field is left, clamped to the range", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<RecordingSection />, harness);
		const days = screen.getByTestId("retention-days");
		await user.clear(days);
		await user.type(days, "14");
		await user.tab();
		expect(
			callsTo(harness.transport, "settings.recording.setRetention"),
		).toEqual([
			{
				method: "settings.recording.setRetention",
				params: { retention: { mode: "keepDays", days: 14 } },
			},
		]);
		await user.clear(days);
		await user.type(days, "0");
		await user.tab();
		expect(
			callsTo(harness.transport, "settings.recording.setRetention").at(-1)
				?.params,
		).toEqual({ retention: { mode: "keepDays", days: 1 } });
	});

	it("asks for a permission that is not granted and offers System Settings when denied", async () => {
		const user = userEvent.setup();
		const recording = await fixture();
		const harness = await createBridgeHarness("", {
			"settings.recording": {
				...recording,
				subtitle: "Permission needed",
				permissions: [
					{ kind: "microphone", state: "denied", isRequesting: false },
					{ kind: "systemAudio", state: "unknown", isRequesting: false },
				],
			} satisfies RecordingSettingsSnapshot,
		});
		renderWithBridge(<RecordingSection />, harness);
		await user.click(screen.getByTestId("permission-microphone-open"));
		expect(callsTo(harness.transport, "system.openSystemSettings")).toEqual([
			{ method: "system.openSystemSettings", params: { kind: "microphone" } },
		]);
		expect(
			screen.getByTestId("permission-systemAudio-request"),
		).toHaveTextContent("Run the test recording");
		await user.click(screen.getByTestId("permission-systemAudio-request"));
		expect(
			callsTo(harness.transport, "settings.recording.requestPermission"),
		).toEqual([
			{
				method: "settings.recording.requestPermission",
				params: { kind: "systemAudio" },
			},
		]);
	});

	it("shows the kept count under Forever and hides the days field", async () => {
		const recording = await fixture();
		const harness = await createBridgeHarness("", {
			"settings.recording": {
				...recording,
				retention: { mode: "keepForever", days: 30 },
				keptForeverCount: 2,
			} satisfies RecordingSettingsSnapshot,
		});
		renderWithBridge(<RecordingSection />, harness);
		expect(screen.queryByTestId("retention-days")).not.toBeInTheDocument();
		expect(screen.getByText("2 recordings kept.")).toBeInTheDocument();
	});
});
