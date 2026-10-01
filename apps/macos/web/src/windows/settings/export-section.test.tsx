import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { ExportSettingsSnapshot } from "@/bridge/contract";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { ExportSection, exportStatus } from "./export-section";

async function fixture(): Promise<ExportSettingsSnapshot> {
	return (await loadFixtureSnapshots())[
		"settings.export"
	] as ExportSettingsSnapshot;
}

describe("ExportSection", () => {
	it("is off by default and turns on through the host", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<ExportSection />, harness);
		expect(screen.getByTestId("export-enabled")).toHaveAttribute(
			"aria-checked",
			"false",
		);
		expect(screen.queryByTestId("choose-vault")).not.toBeInTheDocument();
		expect(screen.queryByTestId("export-status")).not.toBeInTheDocument();
		await user.click(screen.getByTestId("export-enabled"));
		expect(callsTo(harness.transport, "settings.export.setEnabled")).toEqual([
			{ method: "settings.export.setEnabled", params: { value: true } },
		]);
	});

	it("asks for a vault while on without one", async () => {
		const user = userEvent.setup();
		const base = await fixture();
		const harness = await createBridgeHarness("", {
			"settings.export": {
				...base,
				enabled: true,
			} satisfies ExportSettingsSnapshot,
		});
		renderWithBridge(<ExportSection />, harness);
		expect(screen.getByTestId("export-status")).toHaveTextContent(
			"Choose a vault folder to start exporting.",
		);
		await user.click(screen.getByTestId("choose-vault"));
		expect(
			callsTo(harness.transport, "settings.export.chooseVault"),
		).toHaveLength(1);
	});

	it("shows the vault, saves the advanced fields and reports exporting", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=export-on");
		renderWithBridge(<ExportSection />, harness);
		expect(screen.getByTestId("vault-name")).toHaveTextContent("Work Vault");
		expect(screen.getByTestId("export-status")).toHaveTextContent(
			"Exporting to Work Vault.",
		);
		const people = screen.getByTestId("people-folder");
		expect(people).toHaveValue("People");
		await user.clear(people);
		await user.type(people, "Team");
		await user.tab();
		expect(callsTo(harness.transport, "settings.export.update")).toEqual([
			{ method: "settings.export.update", params: { peopleFolder: "Team" } },
		]);
		expect(callsTo(harness.transport, "settings.export.save")).toHaveLength(1);
		await user.click(screen.getByTestId("include-audio"));
		expect(
			callsTo(harness.transport, "settings.export.update").at(-1)?.params,
		).toEqual({ includeAudio: true });
	});

	it("reports a rejected vault as a validation message", () => {
		const status = exportStatus({
			subtitle: "Off",
			enabled: true,
			vaultPath: "/Users/me/gone",
			vaultName: "gone",
			peopleFolder: "",
			includeAudio: false,
			taskTag: "",
			validationMessage: "The vault folder does not exist.",
			saved: false,
		});
		expect(status).toEqual({
			kind: "warning",
			text: "The vault folder does not exist.",
		});
	});
});
