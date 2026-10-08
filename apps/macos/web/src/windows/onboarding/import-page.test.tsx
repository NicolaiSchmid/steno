import { act, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { OnboardingSnapshot } from "@/bridge/contract";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { OnboardingWindow } from "./onboarding-window";

describe("ImportPage", () => {
	it("counts the prompts and names Always Allow, then runs the import", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=onboarding-import");
		renderWithBridge(<OnboardingWindow />, harness);
		expect(screen.getByTestId("onboarding-import")).toBeInTheDocument();
		expect(screen.queryByTestId("onboarding-step")).not.toBeInTheDocument();
		expect(screen.getByTestId("onboarding-intro")).toHaveTextContent(
			"macOS asks for your login password up to two times.",
		);
		expect(screen.getByTestId("import-always-allow")).toHaveTextContent(
			"Choose Always Allow in each prompt.",
		);
		expect(screen.queryByTestId("import-waiting")).not.toBeInTheDocument();
		await user.click(screen.getByTestId("import-run"));
		expect(callsTo(harness.transport, "onboarding.import")).toHaveLength(1);
		await user.click(screen.getByTestId("import-skip"));
		expect(callsTo(harness.transport, "onboarding.skipImport")).toHaveLength(1);
	});

	it("waits for the prompts with both buttons off", async () => {
		const harness = await createBridgeHarness("scenario=onboarding-import");
		renderWithBridge(<OnboardingWindow />, harness);
		const importStep = (await loadFixtureSnapshots())[
			"onboarding.import"
		] as OnboardingSnapshot;
		act(() => {
			harness.transport.emit("onboarding", {
				...importStep,
				swiftImport: { state: "importing", prompts: 2 },
			} satisfies OnboardingSnapshot);
		});
		expect(screen.getByTestId("import-run")).toBeDisabled();
		expect(screen.getByTestId("import-run")).toHaveTextContent(
			"Waiting for macOS…",
		);
		expect(screen.getByTestId("import-skip")).toBeDisabled();
	});

	it("counts a beta's leftover items as prompts", async () => {
		const harness = await createBridgeHarness("scenario=onboarding-import");
		renderWithBridge(<OnboardingWindow />, harness);
		const importStep = (await loadFixtureSnapshots())[
			"onboarding.import"
		] as OnboardingSnapshot;
		act(() => {
			harness.transport.emit("onboarding", {
				...importStep,
				swiftImport: { state: "pending", prompts: 3 },
			} satisfies OnboardingSnapshot);
		});
		expect(screen.getByTestId("onboarding-intro")).toHaveTextContent(
			"macOS asks for your login password up to three times.",
		);
	});

	it("offers Try again after a denied export, with one prompt left", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness(
			"scenario=onboarding-import-waiting",
		);
		renderWithBridge(<OnboardingWindow />, harness);
		expect(screen.getByTestId("onboarding-intro")).toHaveTextContent(
			"macOS asks for your login password once.",
		);
		expect(screen.getByTestId("import-waiting")).toHaveTextContent(
			"macOS did not let Steno read this Mac's phone pairing. Choose Try again, then Always Allow.",
		);
		expect(screen.getByTestId("import-skip")).toHaveTextContent(
			"Continue for now",
		);
		expect(screen.getByTestId("import-always-allow")).toHaveTextContent(
			"Continue for now leaves phone uploads waiting until this step comes back at the next launch.",
		);
		expect(screen.getByTestId("import-always-allow")).not.toHaveTextContent(
			"summaries",
		);
		await user.click(screen.getByTestId("import-run"));
		expect(screen.getByTestId("import-run")).toHaveTextContent("Try again");
		expect(callsTo(harness.transport, "onboarding.import")).toHaveLength(1);
	});
});
