import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { OnboardingSnapshot } from "@/bridge/contract";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { OnboardingWindow } from "./onboarding-window";

/** Page 1 after the handoff (`onboarding.handoff`) with `change` over it. */
async function handoffWith(change: Partial<OnboardingSnapshot>) {
	const handoff = (await loadFixtureSnapshots())[
		"onboarding.handoff"
	] as OnboardingSnapshot;
	return createBridgeHarness("", { onboarding: { ...handoff, ...change } });
}

describe("PermissionsPage", () => {
	it("shows every permission in its state", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<OnboardingWindow />, harness);
		// Granted: the badge, no action.
		expect(screen.getByTestId("permission-microphone")).toHaveTextContent(
			"Allowed",
		);
		expect(
			screen.queryByTestId("permission-microphone-request"),
		).not.toBeInTheDocument();
		// Being requested: the test recording's button waits and says so.
		const systemAudio = screen.getByTestId("permission-systemAudio-request");
		expect(systemAudio).toBeDisabled();
		expect(systemAudio).toHaveTextContent("Asking…");
		expect(screen.getByTestId("permission-systemAudio")).toHaveTextContent(
			"Listening for the test tone",
		);
		// Optional and open: Allow plus Skip, with the badge.
		expect(screen.getByTestId("permission-calendar")).toHaveTextContent(
			"Optional",
		);
		expect(screen.getByTestId("permission-calendar-request")).toHaveTextContent(
			"Allow",
		);
		expect(screen.getByTestId("permission-calendar-skip")).toBeInTheDocument();
		// Skipped: the badge, no action.
		expect(
			screen.getByTestId("permission-localNetwork-skipped"),
		).toHaveTextContent("Skipped");
		expect(
			screen.queryByTestId("permission-localNetwork-skip"),
		).not.toBeInTheDocument();
		// Not every required permission is granted: Later, not Continue.
		expect(screen.getByTestId("onboarding-later")).toBeInTheDocument();
		expect(screen.queryByTestId("onboarding-done")).not.toBeInTheDocument();
	});

	it("requests, skips and acknowledges from a fresh install", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=onboarding-unknown");
		renderWithBridge(<OnboardingWindow />, harness);
		await user.click(screen.getByTestId("permission-microphone-request"));
		expect(callsTo(harness.transport, "onboarding.request")).toEqual([
			{ method: "onboarding.request", params: { kind: "microphone" } },
		]);
		expect(
			screen.queryByTestId("permission-microphone-skip"),
		).not.toBeInTheDocument();
		expect(
			screen.getByTestId("permission-systemAudio-request"),
		).toHaveTextContent("Run the test recording");

		await user.click(screen.getByTestId("permission-calendar-skip"));
		expect(callsTo(harness.transport, "onboarding.skip")).toEqual([
			{ method: "onboarding.skip", params: { kind: "calendar" } },
		]);

		// The local network prompt comes with the first pairing: Got it skips.
		const gotIt = screen.getByTestId("permission-localNetwork-skip");
		expect(gotIt).toHaveTextContent("Got it");
		expect(
			screen.queryByTestId("permission-localNetwork-request"),
		).not.toBeInTheDocument();
		await user.click(gotIt);
		expect(
			callsTo(harness.transport, "onboarding.skip").at(-1)?.params,
		).toEqual({
			kind: "localNetwork",
		});

		await user.click(screen.getByTestId("onboarding-later"));
		expect(callsTo(harness.transport, "onboarding.advance")).toHaveLength(1);
	});

	it("sends a denied permission to System Settings and checks again", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=onboarding-denied");
		renderWithBridge(<OnboardingWindow />, harness);
		await user.click(screen.getByTestId("permission-microphone-open"));
		expect(callsTo(harness.transport, "system.openSystemSettings")).toEqual([
			{ method: "system.openSystemSettings", params: { kind: "microphone" } },
		]);
		await user.click(screen.getByTestId("permission-microphone-check"));
		expect(callsTo(harness.transport, "onboarding.refresh")).toHaveLength(1);
	});

	it("offers Continue once every required permission is granted", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=onboarding-granted");
		renderWithBridge(<OnboardingWindow />, harness);
		expect(screen.queryByTestId("onboarding-later")).not.toBeInTheDocument();
		expect(screen.getAllByText("Allowed")).toHaveLength(4);
		expect(screen.getByTestId("onboarding-done")).toHaveTextContent("Continue");
		await user.click(screen.getByTestId("onboarding-done"));
		expect(callsTo(harness.transport, "onboarding.advance")).toHaveLength(1);
	});

	it("has no speech models or vault row unless the host sends them", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<OnboardingWindow />, harness);
		expect(screen.queryByTestId("onboarding-models")).not.toBeInTheDocument();
		expect(
			screen.queryByTestId("onboarding-vault-access"),
		).not.toBeInTheDocument();
	});

	it("says where a refused vault is allowed, and checks again", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=onboarding-handoff");
		renderWithBridge(<OnboardingWindow />, harness);
		const row = screen.getByTestId("onboarding-vault-access");
		expect(row).toHaveTextContent("Obsidian vault");
		expect(row).toHaveTextContent(
			"Steno cannot write your meeting notes to Work Vault. If macOS asked about the folder and you chose Don't Allow, turn on the folder's switch below Steno in System Settings, under Files & Folders. Otherwise, check that the folder is not read-only. Then choose Check again.",
		);
		expect(
			screen.getByTestId("onboarding-vault-access-message"),
		).toHaveTextContent(
			"The Obsidian vault at /Users/nicolai/Documents/Work Vault is not writable.",
		);
		await user.click(screen.getByTestId("onboarding-vault-open"));
		expect(
			callsTo(harness.transport, "onboarding.openFilesAndFolders"),
		).toHaveLength(1);
		await user.click(screen.getByTestId("onboarding-vault-check"));
		expect(callsTo(harness.transport, "onboarding.refresh")).toHaveLength(1);
	});

	it("shows the download's progress and a writable vault", async () => {
		const harness = await createBridgeHarness(
			"scenario=onboarding-handoff-downloading",
		);
		renderWithBridge(<OnboardingWindow />, harness);
		expect(screen.getByTestId("onboarding-models-progress")).toHaveTextContent(
			"40%",
		);
		expect(
			screen.getByRole("progressbar", {
				name: "Downloading the speech models",
			}),
		).toHaveAttribute("aria-valuenow", "40");
		expect(screen.getByTestId("onboarding-vault-access")).toHaveTextContent(
			"Steno writes your meeting notes to Work Vault.",
		);
	});

	it("starts every model the row lists, in order", async () => {
		const user = userEvent.setup();
		const harness = await handoffWith({
			speechModels: {
				state: "absent",
				detail: "Not downloaded · 518 MB",
				assetIDs: ["parakeetV3", "offlineDiarizer"],
			},
		});
		renderWithBridge(<OnboardingWindow />, harness);
		await user.click(screen.getByTestId("onboarding-models-download"));
		expect(
			callsTo(harness.transport, "settings.transcription.download").map(
				(call) => call.params,
			),
		).toEqual([{ assetID: "parakeetV3" }, { assetID: "offlineDiarizer" }]);
	});

	it("says Installed once the models are in, with nothing more to read", async () => {
		const harness = await handoffWith({
			speechModels: {
				state: "installed",
				detail: "Installed",
				assetIDs: [],
			},
		});
		renderWithBridge(<OnboardingWindow />, harness);
		const row = screen.getByTestId("onboarding-models");
		expect(row).toHaveTextContent("Installed");
		expect(row).not.toHaveTextContent("Meetings wait");
		expect(
			screen.queryByTestId("onboarding-models-download"),
		).not.toBeInTheDocument();
	});

	it("says to choose a vault again that is not there, without System Settings", async () => {
		const user = userEvent.setup();
		const harness = await handoffWith({
			vaultAccess: {
				state: "unusable",
				name: "Work Vault",
				validationMessage:
					"The Obsidian vault at /Volumes/Disk/Work Vault does not exist.",
			},
		});
		renderWithBridge(<OnboardingWindow />, harness);
		expect(screen.getByTestId("onboarding-vault-access")).toHaveTextContent(
			"Steno cannot use Work Vault for your meeting notes. Check that the folder is there, or choose the vault again in Settings > Export.",
		);
		expect(
			screen.getByTestId("onboarding-vault-access-message"),
		).toHaveTextContent("does not exist.");
		expect(
			screen.queryByTestId("onboarding-vault-open"),
		).not.toBeInTheDocument();
		await user.click(screen.getByTestId("onboarding-vault-check"));
		expect(callsTo(harness.transport, "onboarding.refresh")).toHaveLength(1);
	});
});
