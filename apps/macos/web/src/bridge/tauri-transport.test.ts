import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
const listen = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));
vi.mock("@tauri-apps/api/webviewWindow", () => ({
	getCurrentWebviewWindow: () => ({ label: "settings" }),
}));

import { createTauriTransport, hasTauriBridge } from "./tauri-transport";
import { BridgeError } from "./transport";

type Handler = (event: { event: string; id: number; payload: unknown }) => void;

/** Captures the `steno:event` handler so a test can play the host. */
function captureListener() {
	let handler: Handler | undefined;
	listen.mockImplementation(async (_event: string, next: Handler) => {
		handler = next;
		return () => {};
	});
	return (payload: unknown) => {
		if (!handler) {
			throw new Error("listen was not called");
		}
		handler({ event: "steno:event", id: 1, payload });
	};
}

describe("tauri transport", () => {
	beforeEach(() => {
		invoke.mockReset();
		listen.mockReset();
		listen.mockResolvedValue(() => {});
	});

	it("invokes bridge_call with the method and params and returns the result", async () => {
		invoke.mockResolvedValue({ path: "/Users/nicolai/Notes" });
		const transport = createTauriTransport();
		const reply = await transport.call("settings.export.chooseVault", null);
		expect(reply).toEqual({ path: "/Users/nicolai/Notes" });
		expect(invoke).toHaveBeenCalledWith("bridge_call", {
			method: "settings.export.chooseVault",
			params: null,
		});
	});

	it("turns a rejected command into a BridgeError with the host's code", async () => {
		invoke.mockRejectedValue({
			code: "notFound",
			message: "No meeting with that id.",
		});
		const transport = createTauriTransport();
		await expect(
			transport.call("meetings.select", { meetingID: "x" }),
		).rejects.toMatchObject({
			name: "BridgeError",
			code: "notFound",
			method: "meetings.select",
			message: "No meeting with that id.",
		});
	});

	it("maps a rejection outside the contract to the failed code", async () => {
		invoke.mockRejectedValue(new Error("the webview is gone"));
		const transport = createTauriTransport();
		const failure = await transport
			.call("recording.stop", null)
			.catch((cause: unknown) => cause);
		expect(failure).toBeInstanceOf(BridgeError);
		expect(failure).toMatchObject({
			code: "failed",
			message: "the webview is gone",
		});
	});

	it("waits for the event listener before the first command", async () => {
		const order: string[] = [];
		listen.mockImplementation(async () => {
			order.push("listen");
			return () => {};
		});
		invoke.mockImplementation(async () => {
			order.push("invoke");
			return null;
		});
		await createTauriTransport().call("page.ready", null);
		expect(order).toEqual(["listen", "invoke"]);
	});

	it("routes steno:event payloads into subscribers by topic", async () => {
		const emit = captureListener();
		const transport = createTauriTransport();
		await Promise.resolve();
		const seen: unknown[] = [];
		transport.subscribe("recording", (snapshot) => seen.push(snapshot));
		emit({
			topic: "recording",
			payload: { state: "idle", deniedPermissions: [] },
		});
		emit({ topic: "app", payload: { version: "1" } });
		expect(seen).toEqual([{ state: "idle", deniedPermissions: [] }]);
		expect(listen).toHaveBeenCalledWith("steno:event", expect.any(Function), {
			target: { kind: "WebviewWindow", label: "settings" },
		});
	});

	it("detects the Tauri internals on the window", () => {
		expect(hasTauriBridge({} as Window)).toBe(false);
		expect(
			hasTauriBridge({ __TAURI_INTERNALS__: {} } as unknown as Window),
		).toBe(true);
	});
});
