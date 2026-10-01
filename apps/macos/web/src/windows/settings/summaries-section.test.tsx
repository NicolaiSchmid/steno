import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { SummariesSection } from "./summaries-section";

describe("SummariesSection", () => {
	it("shows the preset, the fields and the not-set-up status", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<SummariesSection />, harness);
		expect(screen.getByTestId("preset")).toHaveTextContent(
			"LM Studio on this Mac",
		);
		expect(screen.getByTestId("base-url")).toHaveValue(
			"http://127.0.0.1:1234/v1",
		);
		expect(screen.getByTestId("model")).toHaveAttribute(
			"placeholder",
			"the model loaded in LM Studio",
		);
		expect(screen.getByTestId("api-key")).toHaveAttribute(
			"placeholder",
			"Only if the server needs one",
		);
		expect(screen.getByTestId("summaries-status")).toHaveTextContent(
			/^Not set up\./,
		);
		expect(screen.queryByTestId("test-connection")).not.toBeInTheDocument();
		expect(screen.getByTestId("context-tokens")).toHaveValue("32000");
	});

	it("sends a changed field as one update and a save when it is left", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<SummariesSection />, harness);
		await user.type(screen.getByTestId("model"), "qwen");
		expect(
			callsTo(harness.transport, "settings.summaries.update"),
		).toHaveLength(0);
		await user.tab();
		expect(callsTo(harness.transport, "settings.summaries.update")).toEqual([
			{ method: "settings.summaries.update", params: { model: "qwen" } },
		]);
		expect(callsTo(harness.transport, "settings.summaries.save")).toHaveLength(
			1,
		);
		// An unchanged field saves nothing.
		await user.click(screen.getByTestId("base-url"));
		await user.tab();
		expect(callsTo(harness.transport, "settings.summaries.save")).toHaveLength(
			1,
		);
		await user.type(screen.getByTestId("api-key"), "sk-typed{Enter}");
		expect(
			callsTo(harness.transport, "settings.summaries.update").at(-1)?.params,
		).toEqual({ apiKey: "sk-typed" });
		// The key leaves the field once sent and a later blur sends nothing.
		expect(screen.getByTestId("api-key")).toHaveValue("");
		const updates = callsTo(
			harness.transport,
			"settings.summaries.update",
		).length;
		await user.click(screen.getByTestId("api-key"));
		await user.tab();
		expect(
			callsTo(harness.transport, "settings.summaries.update"),
		).toHaveLength(updates);
	});

	it("shows a connected endpoint with its report and tests again", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=summaries-connected");
		renderWithBridge(<SummariesSection />, harness);
		expect(screen.getByTestId("preset")).toHaveTextContent("OpenAI");
		expect(screen.queryByTestId("base-url")).not.toBeInTheDocument();
		expect(screen.getByTestId("api-key")).toHaveAttribute(
			"placeholder",
			"Saved in your keychain",
		);
		expect(screen.getByTestId("summaries-status")).toHaveTextContent(
			"Connected.",
		);
		await user.click(screen.getByTestId("test-connection"));
		expect(callsTo(harness.transport, "settings.summaries.test")).toHaveLength(
			1,
		);
	});

	it("shows a failed test with the report open", async () => {
		const harness = await createBridgeHarness("scenario=summaries-failed");
		renderWithBridge(<SummariesSection />, harness);
		expect(screen.getByTestId("summaries-status")).toHaveTextContent(
			"Could not connect to the service.",
		);
		expect(screen.getByText(/HTTP 401/)).toBeInTheDocument();
	});

	it("shows the ChatGPT consent card and confirms through the host", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=codex-consent");
		renderWithBridge(<SummariesSection />, harness);
		expect(screen.getByTestId("codex-consent")).toHaveTextContent(
			"Use your ChatGPT plan for summaries",
		);
		expect(screen.getByTestId("codex-signed-in")).toHaveTextContent(
			"nicolai@example.com",
		);
		expect(screen.queryByTestId("context-tokens")).not.toBeInTheDocument();
		await user.click(screen.getByTestId("codex-confirm"));
		expect(
			callsTo(harness.transport, "settings.summaries.confirmCodex"),
		).toHaveLength(1);
	});

	it("shows the confirmed ChatGPT account, its model and the way out", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=codex");
		renderWithBridge(<SummariesSection />, harness);
		expect(screen.getByTestId("codex-account")).toHaveTextContent(
			"Using ChatGPT as nicolai@example.com (Plus).",
		);
		expect(screen.getByTestId("codex-model")).toHaveTextContent(
			"GPT-5.1 Codex",
		);
		await user.click(screen.getByTestId("codex-refresh"));
		expect(
			callsTo(harness.transport, "settings.summaries.refreshCodexModels"),
		).toHaveLength(1);
		await user.click(screen.getByTestId("codex-stop"));
		expect(
			callsTo(harness.transport, "settings.summaries.stopUsingCodex"),
		).toHaveLength(1);
		expect(screen.getByTestId("summaries-status")).toHaveTextContent(
			"Connected.",
		);
	});
});
