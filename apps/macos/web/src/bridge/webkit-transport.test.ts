import { describe, expect, it, vi } from "vitest";
import { BridgeError, SnapshotHub } from "./transport";
import { createWebKitTransport, hasWebKitBridge } from "./webkit-transport";

function fakeWindow(postMessage: (message: unknown) => Promise<unknown>) {
	return {
		webkit: { messageHandlers: { steno: { postMessage } } },
	} as unknown as Window;
}

describe("webkit transport", () => {
	it("posts a request envelope and unwraps the reply result", async () => {
		const postMessage = vi.fn(async (message: unknown) => {
			const request = message as { id: string };
			return { id: request.id, result: { path: "/Users/nicolai/Notes" } };
		});
		const transport = createWebKitTransport(fakeWindow(postMessage));
		const reply = await transport.call("settings.export.chooseVault", null);
		expect(reply).toEqual({ path: "/Users/nicolai/Notes" });
		expect(postMessage).toHaveBeenCalledWith({
			id: expect.stringMatching(/^req-\d+$/),
			method: "settings.export.chooseVault",
			params: null,
		});
	});

	it("turns an error envelope into a BridgeError with the host's code", async () => {
		const transport = createWebKitTransport(
			fakeWindow(async (message) => ({
				id: (message as { id: string }).id,
				error: { code: "notFound", message: "No meeting with that id." },
			})),
		);
		await expect(
			transport.call("meetings.select", { meetingID: "x" }),
		).rejects.toMatchObject({
			name: "BridgeError",
			code: "notFound",
			method: "meetings.select",
		});
	});

	it("rejects a reply whose id does not match and a malformed reply", async () => {
		const mismatched = createWebKitTransport(
			fakeWindow(async () => ({ id: "other", result: null })),
		);
		await expect(
			mismatched.call("recording.stop", null),
		).rejects.toBeInstanceOf(BridgeError);
		const malformed = createWebKitTransport(
			fakeWindow(async () => ({ error: null })),
		);
		await expect(malformed.call("recording.stop", null)).rejects.toBeInstanceOf(
			BridgeError,
		);
	});

	it("routes window.steno.emit into subscribers", async () => {
		const target = fakeWindow(async () => ({ id: "req-1" }));
		const transport = createWebKitTransport(target);
		const seen: unknown[] = [];
		transport.subscribe("recording", (snapshot) => seen.push(snapshot));
		target.steno?.emit("recording", { state: "idle", deniedPermissions: [] });
		expect(seen).toEqual([{ state: "idle", deniedPermissions: [] }]);
		expect(hasWebKitBridge(target)).toBe(true);
	});

	it("keeps delivering a snapshot when one subscriber throws", () => {
		const hub = new SnapshotHub();
		const error = vi.spyOn(console, "error").mockImplementation(() => {});
		hub.subscribe("app", () => {
			throw new Error("boom");
		});
		const second = vi.fn();
		hub.subscribe("app", second);
		hub.emit("app", { version: "1" });
		expect(second).toHaveBeenCalledWith({ version: "1" });
		expect(error).toHaveBeenCalled();
		error.mockRestore();
	});
});
