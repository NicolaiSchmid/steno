import { act, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { OnboardingSnapshot } from "@/bridge/contract";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { OnboardingWindow } from "./onboarding-window";

describe("OnboardingWindow", () => {
	it("tells the host the page is ready once and shows the host's page", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<OnboardingWindow />, harness);
		expect(callsTo(harness.transport, "page.ready")).toHaveLength(1);
		expect(screen.getByTestId("onboarding-permissions")).toBeInTheDocument();
		expect(screen.queryByTestId("onboarding-setup")).not.toBeInTheDocument();
	});

	it("follows the host from page 1 to page 2 and back", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<OnboardingWindow />, harness);
		const onboarding = (await loadFixtureSnapshots())
			.onboarding as OnboardingSnapshot;
		act(() => {
			harness.transport.emit("onboarding", {
				...onboarding,
				page: "setup",
			} satisfies OnboardingSnapshot);
		});
		expect(screen.getByTestId("onboarding-setup")).toBeInTheDocument();
		expect(
			screen.queryByTestId("onboarding-permissions"),
		).not.toBeInTheDocument();
		act(() => {
			harness.transport.emit("onboarding", onboarding);
		});
		expect(screen.getByTestId("onboarding-permissions")).toBeInTheDocument();
	});

	it("asks the host to close the window once onboarding finished, once", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<OnboardingWindow />, harness);
		expect(callsTo(harness.transport, "window.close")).toHaveLength(0);
		const onboarding = (await loadFixtureSnapshots())
			.onboarding as OnboardingSnapshot;
		const finished = {
			...onboarding,
			page: "setup",
			finished: true,
		} satisfies OnboardingSnapshot;
		act(() => {
			harness.transport.emit("onboarding", finished);
		});
		expect(callsTo(harness.transport, "window.close")).toEqual([
			{ method: "window.close", params: { window: "onboarding" } },
		]);
		act(() => {
			harness.transport.emit("onboarding", { ...finished });
		});
		expect(callsTo(harness.transport, "window.close")).toHaveLength(1);
	});
});
