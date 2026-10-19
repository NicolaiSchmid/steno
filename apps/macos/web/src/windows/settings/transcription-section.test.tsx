import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { TranscriptionSection } from "./transcription-section";

describe("TranscriptionSection", () => {
	it("shows the engine picker, an installed component and one downloading", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<TranscriptionSection />, harness);
		expect(screen.getByTestId("engine")).toHaveTextContent("Parakeet");
		expect(screen.getByTestId("asset-parakeetV3")).toHaveTextContent(
			"Parakeet v3",
		);
		expect(screen.getByTestId("asset-parakeetV3-remove")).toBeInTheDocument();
		expect(
			screen.getByTestId("asset-offlineDiarizer-progress"),
		).toHaveTextContent("35%");
		expect(screen.getByRole("progressbar")).toBeInTheDocument();
		expect(
			screen.getByText(
				"Downloads happen once and are kept for later meetings.",
			),
		).toBeInTheDocument();
	});

	it("removes an installed component through the host", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<TranscriptionSection />, harness);
		await user.click(screen.getByTestId("asset-parakeetV3-remove"));
		expect(callsTo(harness.transport, "settings.transcription.remove")).toEqual(
			[
				{
					method: "settings.transcription.remove",
					params: { assetID: "parakeetV3" },
				},
			],
		);
	});

	it("offers a retry with the failure folded away when a download failed", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=download-failed");
		renderWithBridge(<TranscriptionSection />, harness);
		expect(
			screen.getByText("The download did not finish."),
		).toBeInTheDocument();
		await user.click(screen.getByTestId("asset-parakeetV3-failure"));
		expect(screen.getByText(/notConnectedToInternet/)).toBeInTheDocument();
		await user.click(screen.getByTestId("asset-parakeetV3-retry"));
		expect(
			callsTo(harness.transport, "settings.transcription.download"),
		).toEqual([
			{
				method: "settings.transcription.download",
				params: { assetID: "parakeetV3" },
			},
		]);
	});
});
