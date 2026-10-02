import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { bridgeError } from "./contract";
import { BridgeError, type BridgeTransport, SnapshotHub } from "./transport";

/**
 * The transport inside the Tauri shell (`apps/desktop`, plan
 * `.plans/2026-10-02-rust-core-and-tauri-shell.md`, invariant 1). Commands
 * go through the `bridge_call` command as `{ method, params }` and resolve
 * with the host's result; a rejected command carries the contract's
 * `BridgeError` shape (`code`, `message`). Snapshots arrive as `steno:event`
 * events whose payload is the contract's event envelope (`topic`,
 * `payload`), one per coalesced change, and are fanned out by a
 * `SnapshotHub` exactly as the WebKit transport does. The listener is
 * scoped to this webview window: the host emits each window's snapshots to
 * that window's label, and a listener for any target would also hear the
 * other windows'. Nothing above this module knows which shell it runs in.
 */

interface TauriWindow {
	__TAURI_INTERNALS__?: unknown;
}

interface BridgeEventPayload {
	topic: string;
	payload: unknown;
}

export const BRIDGE_CALL_COMMAND = "bridge_call";
export const BRIDGE_EVENT_NAME = "steno:event";

export function hasTauriBridge(target: Window = window): boolean {
	return Boolean((target as TauriWindow).__TAURI_INTERNALS__);
}

export function createTauriTransport(): BridgeTransport {
	const hub = new SnapshotHub();
	// Registered before the first command leaves: the host publishes its
	// first snapshots in reply to `page.ready`, and a listener that is still
	// being installed would miss them.
	const { label } = getCurrentWebviewWindow();
	const listening = listen<BridgeEventPayload>(
		BRIDGE_EVENT_NAME,
		(event) => {
			const { topic, payload } = event.payload;
			hub.emit(topic, payload);
		},
		{ target: { kind: "WebviewWindow", label } },
	).catch((cause: unknown) => {
		console.error("bridge: listening for steno:event failed", cause);
	});

	return {
		async call(method: string, params: unknown): Promise<unknown> {
			await listening;
			try {
				return await invoke<unknown>(BRIDGE_CALL_COMMAND, {
					method,
					params: params ?? null,
				});
			} catch (cause) {
				const parsed = bridgeError.safeParse(cause);
				if (parsed.success) {
					throw new BridgeError(method, parsed.data.code, parsed.data.message);
				}
				const text = cause instanceof Error ? cause.message : String(cause);
				throw new BridgeError(method, "failed", text);
			}
		},
		subscribe: hub.subscribe.bind(hub),
	};
}
