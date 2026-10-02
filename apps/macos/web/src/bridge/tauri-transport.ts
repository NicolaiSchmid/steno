import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { bridgeError } from "./contract";
import { BridgeError, type BridgeTransport, SnapshotHub } from "./transport";

/**
 * The transport inside the Tauri shell (`apps/desktop`). Commands go
 * through the `bridge_call` command as `{ method, params }`; a rejection
 * carries the contract's `BridgeError` shape. Snapshots arrive as
 * `steno:event` events carrying the event envelope (`topic`, `payload`) and
 * fan out through a `SnapshotHub` like the WebKit transport's. The listener
 * is scoped to this window's label: the host emits each window's snapshots
 * to that window, and an unscoped listener would hear the others' too.
 */

/**
 * What the shell injects. The window label is read from the metadata
 * instead of `getCurrentWebviewWindow()` so the window API stays out of the
 * WebKit bundle.
 */
interface TauriWindow {
	__TAURI_INTERNALS__?: {
		metadata?: { currentWebview?: { label?: string } };
	};
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

/** This window's label, as the shell created it (`windows.rs`). */
export function currentWindowLabel(target: Window = window): string {
	const label = (target as TauriWindow).__TAURI_INTERNALS__?.metadata
		?.currentWebview?.label;
	if (!label) {
		throw new Error("bridge: the Tauri window label is missing");
	}
	return label;
}

export function createTauriTransport(): BridgeTransport {
	const hub = new SnapshotHub();
	// Registered before the first command leaves: the host publishes its
	// first snapshots in reply to `page.ready`, and a listener that is still
	// being installed would miss them.
	const label = currentWindowLabel();
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
