import { screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { AppSnapshot } from "@/bridge/contract";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { MainWindow } from "./main-window";

const FAILED = "00000000-0000-0000-0000-000000000044";

describe("MainWindow", () => {
	it("tells the host the page is ready, once", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<MainWindow />, harness);
		expect(screen.getByTestId("main-window")).toBeInTheDocument();
		expect(callsTo(harness.transport, "page.ready")).toHaveLength(1);
	});

	it("leaves a deep link for the host to follow", async () => {
		const app = (await loadFixtureSnapshots()).app as AppSnapshot;
		const harness = await createBridgeHarness("", {
			app: { ...app, requestedMeetingID: FAILED } satisfies AppSnapshot,
		});
		renderWithBridge(<MainWindow />, harness);
		expect(screen.getByTestId("main-window")).toBeInTheDocument();
		expect(callsTo(harness.transport, "meetings.select")).toHaveLength(0);
		expect(harness.transport.calls.map((call) => call.method)).toEqual([
			"page.ready",
		]);
	});
});
