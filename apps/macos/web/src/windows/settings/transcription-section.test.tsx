import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { TranscriptionSection } from "./transcription-section";

describe("TranscriptionSection", () => {
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
