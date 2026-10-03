import { screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { isPanelRoute, PanelWindow } from "./panel-window";

describe("PanelWindow", () => {
	it("knows the two panel routes and nothing else", () => {
		expect(isPanelRoute("/panel/bubble")).toBe(true);
		expect(isPanelRoute("/panel/prompt")).toBe(true);
		expect(isPanelRoute("/main")).toBe(false);
		expect(isPanelRoute("/panel")).toBe(false);
		expect(isPanelRoute("/panel/other")).toBe(false);
	});

	it("renders the bubble at its route", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(
			<PanelWindow params={new URLSearchParams()} route="/panel/bubble" />,
			harness,
		);
		expect(await screen.findByTestId("recording-bubble")).toBeInTheDocument();
	});

	it("renders the prompt with the request from the query", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(
			<PanelWindow
				params={new URLSearchParams("app=FaceTime&seconds=30")}
				route="/panel/prompt"
			/>,
			harness,
		);
		expect(screen.getByTestId("prompt-title")).toHaveTextContent(
			"FaceTime opened the microphone",
		);
	});
});
