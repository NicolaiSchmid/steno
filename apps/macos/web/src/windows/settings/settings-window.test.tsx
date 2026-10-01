import { act, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { AppSnapshot } from "@/bridge/contract";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { SettingsWindow } from "./settings-window";

describe("SettingsWindow", () => {
	it("tells the host the page is ready once and opens on General", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<SettingsWindow />, harness);
		expect(callsTo(harness.transport, "page.ready")).toHaveLength(1);
		expect(screen.getByTestId("section-title-general")).toHaveTextContent(
			"General",
		);
	});

	it("lists the six sections with the subtitles their snapshots carry", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<SettingsWindow />, harness);
		expect(screen.getByTestId("settings-general")).toHaveTextContent(
			"Steno 0.10.0",
		);
		expect(screen.getByTestId("settings-recording")).toHaveTextContent("Ready");
		expect(screen.getByTestId("settings-transcription")).toHaveTextContent(
			"Ready",
		);
		expect(screen.getByTestId("settings-summaries")).toHaveTextContent(
			"Not set up",
		);
		expect(screen.getByTestId("settings-export")).toHaveTextContent("Off");
		expect(screen.getByTestId("settings-iphone")).toHaveTextContent(
			"Nicolai's iPhone",
		);
		expect(screen.getByTestId("settings-general")).toHaveAttribute(
			"aria-current",
			"true",
		);
	});

	it("switches sections from the sidebar", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<SettingsWindow />, harness);
		await user.click(screen.getByTestId("settings-recording"));
		expect(screen.getByTestId("section-title-recording")).toBeInTheDocument();
		expect(screen.queryByTestId("section-general")).not.toBeInTheDocument();
	});

	it("opens on the route's section", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<SettingsWindow section="export" />, harness);
		expect(screen.getByTestId("section-title-export")).toBeInTheDocument();
	});

	it("follows the host's deep link when it arrives", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<SettingsWindow />, harness);
		const app = (await loadFixtureSnapshots()).app as AppSnapshot;
		act(() => {
			harness.transport.emit("app", {
				...app,
				requestedSettingsSection: "summaries",
			} satisfies AppSnapshot);
		});
		expect(screen.getByTestId("section-title-summaries")).toBeInTheDocument();
		// The host clears the request; the page keeps its selection.
		act(() => {
			harness.transport.emit("app", app);
		});
		expect(screen.getByTestId("section-title-summaries")).toBeInTheDocument();
	});

	it("follows the same deep link twice, after the user moved away", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<SettingsWindow />, harness);
		const app = (await loadFixtureSnapshots()).app as AppSnapshot;
		const toRecording = {
			...app,
			requestedSettingsSection: "recording",
		} satisfies AppSnapshot;
		act(() => {
			harness.transport.emit("app", toRecording);
		});
		expect(screen.getByTestId("section-title-recording")).toBeInTheDocument();
		act(() => {
			harness.transport.emit("app", app);
		});
		await user.click(screen.getByTestId("settings-general"));
		expect(screen.getByTestId("section-title-general")).toBeInTheDocument();
		// The host publishes the same request again: a new snapshot, shown again.
		act(() => {
			harness.transport.emit("app", { ...toRecording });
		});
		expect(screen.getByTestId("section-title-recording")).toBeInTheDocument();
	});
});
