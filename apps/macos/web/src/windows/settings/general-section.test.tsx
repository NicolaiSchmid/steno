import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { GeneralSettingsSnapshot } from "@/bridge/contract";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { GeneralSection } from "./general-section";

describe("GeneralSection", () => {
	it("shows the version, the update status and the template's description", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<GeneralSection />, harness);
		expect(screen.getByText("Steno 0.10.0")).toBeInTheDocument();
		expect(screen.getByTestId("update-status")).toHaveTextContent(
			/^Up to date, checked /,
		);
		expect(screen.getByTestId("default-template")).toHaveTextContent(
			"Standard",
		);
		expect(
			screen.getByText(
				"Executive summary, decisions, open questions and the tasks.",
			),
		).toBeInTheDocument();
		expect(screen.getByTestId("permission-calendar")).toHaveTextContent(
			"Allowed",
		);
	});

	it("sends the switches and the update check to the host", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<GeneralSection />, harness);
		await user.click(screen.getByTestId("detection"));
		expect(
			callsTo(harness.transport, "settings.general.setDetectionEnabled"),
		).toEqual([
			{
				method: "settings.general.setDetectionEnabled",
				params: { value: false },
			},
		]);
		await user.click(screen.getByTestId("launch-at-login"));
		expect(
			callsTo(harness.transport, "settings.general.setLaunchAtLogin"),
		).toEqual([
			{ method: "settings.general.setLaunchAtLogin", params: { value: false } },
		]);
		await user.click(screen.getByTestId("auto-install"));
		expect(
			callsTo(harness.transport, "settings.general.setAutomaticUpdates"),
		).toEqual([
			{
				method: "settings.general.setAutomaticUpdates",
				params: { automaticallyChecks: true, automaticallyDownloads: true },
			},
		]);
		await user.click(screen.getByTestId("check-for-updates"));
		expect(callsTo(harness.transport, "updates.check")).toHaveLength(1);
	});

	it("shows the error with its details, the approval notice and the update", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=settings-error");
		renderWithBridge(<GeneralSection />, harness);
		expect(screen.getByTestId("section-error")).toHaveTextContent(
			"The setting could not be saved.",
		);
		await user.click(screen.getByTestId("section-error-details"));
		expect(screen.getByText(/SettingsStoreError/)).toBeInTheDocument();
		expect(screen.getByTestId("login-item-approval")).toBeInTheDocument();
		await user.click(screen.getByTestId("open-login-items"));
		expect(
			callsTo(harness.transport, "settings.general.openLoginItems"),
		).toHaveLength(1);
		expect(screen.getByTestId("update-status")).toHaveTextContent(
			"Update available: 0.10.1",
		);
	});

	it("shows a packaged install's login item locked and no update check", async () => {
		const fixture = (await loadFixtureSnapshots())[
			"settings.general"
		] as GeneralSettingsSnapshot;
		const loginItemNote =
			"Your system opens Steno when you log in and manages this setting.";
		const managedNote = "Updates come from your package manager.";
		const harness = await createBridgeHarness("", {
			"settings.general": {
				...fixture,
				loginItemNote,
				updates: { ...fixture.updates, canCheck: false, managedNote },
			} satisfies GeneralSettingsSnapshot,
		});
		renderWithBridge(<GeneralSection />, harness);
		expect(screen.getByTestId("launch-at-login")).toHaveAttribute(
			"aria-disabled",
			"true",
		);
		expect(screen.getByTestId("launch-at-login")).toHaveAttribute(
			"aria-checked",
			"true",
		);
		await userEvent.setup().click(screen.getByTestId("launch-at-login"));
		expect(
			callsTo(harness.transport, "settings.general.setLaunchAtLogin"),
		).toEqual([]);
		expect(screen.getByText(loginItemNote)).toBeInTheDocument();
		expect(screen.getByTestId("update-status")).toHaveTextContent(managedNote);
		expect(screen.queryByTestId("check-for-updates")).not.toBeInTheDocument();
		expect(screen.queryByTestId("auto-check")).not.toBeInTheDocument();
		expect(screen.queryByTestId("auto-install")).not.toBeInTheDocument();
	});

	it("opens the acknowledgements and lists models and libraries", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<GeneralSection />, harness);
		await user.click(screen.getByTestId("acknowledgements"));
		const dialog = await screen.findByTestId("acknowledgements-dialog");
		expect(dialog).toHaveTextContent("Parakeet TDT 0.6B v3 (int8)");
		expect(dialog).toHaveTextContent("Sparkle");
		expect(dialog).toHaveTextContent("MIT");
	});
});
