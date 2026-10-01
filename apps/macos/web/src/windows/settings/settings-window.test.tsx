import { act, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { AppSnapshot } from "@/bridge/contract";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { SettingsWindow } from "./settings-window";

describe("SettingsWindow", () => {
	it("tells the host the page is ready once and which section it shows", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<SettingsWindow />, harness);
		expect(callsTo(harness.transport, "page.ready")).toHaveLength(1);
		expect(callsTo(harness.transport, "settings.showSection")).toEqual([
			{ method: "settings.showSection", params: { section: "general" } },
		]);
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

	it("switches sections from the sidebar and reports each", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<SettingsWindow />, harness);
		await user.click(screen.getByTestId("settings-recording"));
		expect(screen.getByTestId("section-title-recording")).toBeInTheDocument();
		expect(screen.queryByTestId("section-general")).not.toBeInTheDocument();
		expect(callsTo(harness.transport, "settings.showSection").at(-1)).toEqual({
			method: "settings.showSection",
			params: { section: "recording" },
		});
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
		expect(callsTo(harness.transport, "settings.showSection").at(-1)).toEqual({
			method: "settings.showSection",
			params: { section: "summaries" },
		});
		// The host clears the request; the page keeps its selection.
		act(() => {
			harness.transport.emit("app", app);
		});
		expect(screen.getByTestId("section-title-summaries")).toBeInTheDocument();
	});

	it("reports a deep link to the section already shown, and the same link twice", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<SettingsWindow />, harness);
		const app = (await loadFixtureSnapshots()).app as AppSnapshot;
		const before = callsTo(harness.transport, "settings.showSection").length;
		act(() => {
			harness.transport.emit("app", {
				...app,
				requestedSettingsSection: "general",
			} satisfies AppSnapshot);
		});
		expect(callsTo(harness.transport, "settings.showSection")).toHaveLength(
			before + 1,
		);
		act(() => {
			harness.transport.emit("app", app);
		});
		act(() => {
			harness.transport.emit("app", {
				...app,
				requestedSettingsSection: "general",
			} satisfies AppSnapshot);
		});
		expect(callsTo(harness.transport, "settings.showSection")).toHaveLength(
			before + 2,
		);
		expect(
			callsTo(harness.transport, "settings.showSection").at(-1)?.params,
		).toEqual({
			section: "general",
		});
	});
});
