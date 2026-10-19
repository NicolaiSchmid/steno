import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { createBridgeClient } from "./client";
import { BridgeProvider, useSnapshot } from "./hooks";
import { createMockTransport, loadFixtureSnapshots } from "./mock-transport";

async function harness() {
	const transport = createMockTransport({ snapshots: loadFixtureSnapshots });
	await transport.ready();
	const client = createBridgeClient(transport);
	const wrapper = ({ children }: { children: React.ReactNode }) => (
		<BridgeProvider client={client}>{children}</BridgeProvider>
	);
	return { transport, wrapper };
}

describe("useSnapshot", () => {
	it("delivers the parsed fixture snapshot and every later one", async () => {
		const { transport, wrapper } = await harness();
		const { result } = renderHook(() => useSnapshot("recording"), {
			wrapper,
		});
		expect(result.current?.state).toBe("idle");

		act(() => {
			transport.emit("recording", { state: "starting", deniedPermissions: [] });
		});
		expect(result.current?.state).toBe("starting");
	});
});
