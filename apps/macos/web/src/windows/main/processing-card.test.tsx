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

	it.each([
		[MODELS_MISSING_STAGE, "Download the speech models in Settings"],
		[STORE_BUSY_STAGE, "Waiting for another app to finish with Steno's data"],
	])(
		"shows the host's words for a meeting waiting at %s",
		async (stage, title) => {
			const harness = await createBridgeHarness();
			renderWithBridge(
				<ProcessingCard
					entry={{ meetingID: MEETING, stage, title, fraction: 0 }}
					state="queued"
				/>,
				harness,
			);
			expect(screen.getByTestId("processing-stage")).toHaveTextContent(title);
			expect(
				screen.queryByTestId("processing-remaining"),
			).not.toBeInTheDocument();
		},
	);
});
