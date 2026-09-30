import { act, render, renderHook, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { createBridgeClient } from "./client";
import { BridgeProvider, useBridge, useSnapshot } from "./hooks";
import { createMockTransport, loadFixtureSnapshots } from "./mock-transport";

async function harness() {
	const transport = createMockTransport({ snapshots: loadFixtureSnapshots });
	await transport.ready();
	const client = createBridgeClient(transport);
	const wrapper = ({ children }: { children: React.ReactNode }) => (
		<BridgeProvider client={client}>{children}</BridgeProvider>
	);
	return { transport, client, wrapper };
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

	it("drops a snapshot that violates the contract", async () => {
		const { transport, wrapper } = await harness();
		const spy = vi.spyOn(console, "error").mockImplementation(() => undefined);
		const { result } = renderHook(() => useSnapshot("recording"), {
			wrapper,
		});
		act(() => {
			transport.emit("recording", { state: "flying" });
		});
		expect(result.current?.state).toBe("idle");
		expect(spy).toHaveBeenCalled();
		spy.mockRestore();
	});

	it("unsubscribes on unmount", async () => {
		const { transport, wrapper } = await harness();
		const unsubscribe = vi.fn();
		const subscribe = transport.subscribe.bind(transport);
		transport.subscribe = ((topic, handler) => {
			const stop = subscribe(topic, handler);
			return () => {
				unsubscribe();
				stop();
			};
		}) as typeof transport.subscribe;
		const client = createBridgeClient(transport);

		function Probe() {
			const app = useSnapshot("app");
			return <span>{app?.version ?? "none"}</span>;
		}
		const { unmount } = render(
			<BridgeProvider client={client}>
				<Probe />
			</BridgeProvider>,
			{ wrapper },
		);
		expect(screen.getByText("0.10.0")).toBeInTheDocument();
		unmount();
		expect(unsubscribe).toHaveBeenCalledTimes(1);
	});
});

describe("useBridge", () => {
	it("returns the provided client", async () => {
		const { client, wrapper } = await harness();
		const { result } = renderHook(() => useBridge(), { wrapper });
		expect(result.current).toBe(client);
	});
});
