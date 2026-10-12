import { screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { createBridgeHarness, renderWithBridge } from "@/test/bridge";
import {
	MODELS_MISSING_STAGE,
	ProcessingCard,
	STORE_BUSY_STAGE,
} from "./processing-card";

const MEETING = "00000000-0000-0000-0000-000000000002";

describe("ProcessingCard", () => {
	it("tells a queued meeting it starts after the current one", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(
			<ProcessingCard entry={undefined} state="queued" />,
			harness,
		);
		expect(screen.getByTestId("processing-stage")).toHaveTextContent(
			"Waiting to process",
		);
		expect(screen.getByTestId("processing-remaining")).toHaveTextContent(
			"starts when the current meeting finishes",
		);
	});

	it("says what to do for a meeting that waits for a model download", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(
			<ProcessingCard
				entry={{
					meetingID: MEETING,
					stage: MODELS_MISSING_STAGE,
					title: "Download the speech models in Settings",
					fraction: 0,
				}}
				state="queued"
			/>,
			harness,
		);
		expect(screen.getByTestId("processing-stage")).toHaveTextContent(
			"Download the speech models in Settings",
		);
		expect(
			screen.queryByTestId("processing-remaining"),
		).not.toBeInTheDocument();
	});

	it("says what a meeting waits for when another app holds the library", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(
			<ProcessingCard
				entry={{
					meetingID: MEETING,
					stage: STORE_BUSY_STAGE,
					title: "Waiting for another app to finish with Steno's library",
					fraction: 0,
				}}
				state="queued"
			/>,
			harness,
		);
		expect(screen.getByTestId("processing-stage")).toHaveTextContent(
			"Waiting for another app to finish with Steno's library",
		);
		expect(
			screen.queryByTestId("processing-remaining"),
		).not.toBeInTheDocument();
	});
});
