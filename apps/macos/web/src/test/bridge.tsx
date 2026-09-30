import { type RenderResult, render } from "@testing-library/react";
import type { ReactElement } from "react";
import { type BridgeClient, createBridgeClient } from "@/bridge/client";
import { BridgeProvider } from "@/bridge/hooks";
import {
	applyScenario,
	createMockTransport,
	type FixtureMap,
	loadFixtureReplies,
	loadFixtureSnapshots,
	type MockTransport,
} from "@/bridge/mock-transport";
import { TooltipProvider } from "@/components/ui";

export interface BridgeHarness {
	transport: MockTransport;
	client: BridgeClient;
}

/**
 * A client over the mock transport with the fixture snapshots (bent by
 * `query`, as the page's `?scenario=` would, then by `overrides`) and the
 * fixture replies (`replyOverrides` win, per method), ready before it
 * returns so the first render sees the data.
 */
export async function createBridgeHarness(
	query = "",
	overrides: FixtureMap = {},
	replyOverrides: FixtureMap = {},
): Promise<BridgeHarness> {
	const replies = { ...(await loadFixtureReplies()), ...replyOverrides };
	const transport = createMockTransport({
		snapshots: async () => ({
			...applyScenario(
				await loadFixtureSnapshots(),
				new URLSearchParams(query),
			),
			...overrides,
		}),
		replies,
	});
	await transport.ready();
	return { transport, client: createBridgeClient(transport) };
}

/** Renders `ui` under the harness's client. */
export function renderWithBridge(
	ui: ReactElement,
	harness: BridgeHarness,
): RenderResult {
	return render(
		<TooltipProvider delay={0}>
			<BridgeProvider client={harness.client}>{ui}</BridgeProvider>
		</TooltipProvider>,
	);
}

/** The recorded calls of one method. */
export function callsTo(transport: MockTransport, method: string) {
	return transport.calls.filter((call) => call.method === method);
}
