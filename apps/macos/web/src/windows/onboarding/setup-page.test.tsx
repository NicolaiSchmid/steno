import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { OnboardingWindow } from "./onboarding-window";

describe("SetupPage", () => {
	it("opens both rows with the endpoint form and the vault chooser", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=onboarding-setup-open");
		renderWithBridge(<OnboardingWindow />, harness);
		expect(screen.getByTestId("onboarding-save-summaries")).toBeDisabled();
		expect(screen.getByTestId("onboarding-test-summaries")).toBeEnabled();
		expect(
			screen.queryByTestId("onboarding-save-vault"),
		).not.toBeInTheDocument();

		await user.type(screen.getByTestId("onboarding-model"), "qwen");
		expect(
			callsTo(harness.transport, "settings.summaries.update"),
		).toHaveLength(0);
		await user.tab();
		expect(callsTo(harness.transport, "settings.summaries.update")).toEqual([
			{ method: "settings.summaries.update", params: { model: "qwen" } },
		]);
		expect(callsTo(harness.transport, "onboarding.saveSummaries")).toHaveLength(
			0,
		);
		await user.type(
			screen.getByTestId("onboarding-api-key"),
			"sk-typed{Enter}",
		);
		expect(
			callsTo(harness.transport, "settings.summaries.update").at(-1)?.params,
		).toEqual({ apiKey: "sk-typed" });
		// Nothing is stored before Save, so the masked draft stays in the field.
		expect(screen.getByTestId("onboarding-api-key")).toHaveValue("sk-typed");

		await user.click(screen.getByTestId("onboarding-test-summaries"));
		expect(callsTo(harness.transport, "settings.summaries.test")).toHaveLength(
			1,
		);
		await user.click(screen.getByTestId("onboarding-choose-vault"));
		expect(callsTo(harness.transport, "onboarding.chooseVault")).toHaveLength(
			1,
		);
		await user.click(screen.getByTestId("setup-vault-skip"));
		expect(callsTo(harness.transport, "onboarding.skipSetup")).toEqual([
			{ method: "onboarding.skipSetup", params: { step: "vault" } },
		]);
		await user.click(screen.getByTestId("setup-summaries-skip"));
		expect(
			callsTo(harness.transport, "onboarding.skipSetup").at(-1)?.params,
		).toEqual({ step: "summaries" });

		await user.click(screen.getByTestId("onboarding-back"));
		expect(callsTo(harness.transport, "onboarding.back")).toHaveLength(1);
		await user.click(screen.getByTestId("onboarding-finish"));
		expect(callsTo(harness.transport, "onboarding.finish")).toHaveLength(1);
	});

	it("collapses a saved row and shows why a vault was refused", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=onboarding-setup");
		renderWithBridge(<OnboardingWindow />, harness);
		expect(screen.getByTestId("setup-summaries-saved")).toHaveTextContent(
			"Saved: qwen3-8b at 127.0.0.1",
		);
		expect(screen.queryByTestId("onboarding-preset")).not.toBeInTheDocument();
		expect(screen.getByTestId("onboarding-vault-name")).toHaveTextContent(
			"Work Vault",
		);
		expect(screen.getByTestId("onboarding-vault-validation")).toHaveTextContent(
			"does not exist",
		);
		expect(screen.getByTestId("onboarding-choose-vault")).toHaveTextContent(
			"Choose another…",
		);
		await user.click(screen.getByTestId("onboarding-save-vault"));
		expect(callsTo(harness.transport, "onboarding.saveVault")).toHaveLength(1);
	});

	it("shows the ChatGPT consent card and confirms through it", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=onboarding-codex");
		renderWithBridge(<OnboardingWindow />, harness);
		expect(screen.getByTestId("onboarding-preset")).toHaveTextContent(
			"ChatGPT (Codex)",
		);
		expect(screen.queryByTestId("codex-check-again")).not.toBeInTheDocument();
		expect(screen.queryByTestId("onboarding-model")).not.toBeInTheDocument();
		await user.click(screen.getByTestId("codex-confirm"));
		expect(
			callsTo(harness.transport, "onboarding.confirmSummariesWithCodex"),
		).toHaveLength(1);
		// The card carries the row's own Skip.
		await user.click(screen.getByTestId("setup-summaries-skip"));
		expect(callsTo(harness.transport, "onboarding.skipSetup")).toEqual([
			{ method: "onboarding.skipSetup", params: { step: "summaries" } },
		]);
	});

	it("shows the saved vault beside the open Summaries form", async () => {
		const harness = await createBridgeHarness(
			"scenario=onboarding-vault-saved",
		);
		renderWithBridge(<OnboardingWindow />, harness);
		expect(screen.getByTestId("setup-vault-saved")).toHaveTextContent(
			"Saved: Work Vault",
		);
		expect(
			screen.queryByTestId("onboarding-choose-vault"),
		).not.toBeInTheDocument();
		expect(screen.getByTestId("onboarding-preset")).toBeInTheDocument();
	});
});
