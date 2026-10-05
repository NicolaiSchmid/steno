import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { OnboardingWindow } from "./onboarding-window";

describe("PermissionsPage", () => {
	it("shows every permission in its state with the retention sentence", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<OnboardingWindow />, harness);
		expect(screen.getByTestId("onboarding-step")).toHaveTextContent(
			"Step 1 of 2",
		);
		expect(screen.getByTestId("onboarding-title")).toHaveTextContent(
			"Welcome to Steno",
		);
		expect(screen.getByTestId("onboarding-intro")).toHaveTextContent(
			"A few permissions, then where summaries come from and where meetings go. Audio never leaves this Mac.",
		);
		expect(screen.getByTestId("onboarding-retention")).toHaveTextContent(
			/^Each recording is deleted 30 days/,
		);
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
});
