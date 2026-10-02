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

export function createTauriTransport(target: Window = window): BridgeTransport {
	const hub = new SnapshotHub();
	// Registered before the first command leaves: the host publishes its
	// first snapshots in reply to `page.ready`, and a listener that is still
	// being installed would miss them.
	const label = currentWindowLabel(target);
	const listening = listen<BridgeEventPayload>(
		BRIDGE_EVENT_NAME,
		(event) => {
			const { topic, payload } = event.payload;
			hub.emit(topic, payload);
		},
		{ target: { kind: "WebviewWindow", label } },
	);
	// A failure surfaces through the first `call` below; this branch only
	// keeps it from being reported as unhandled before that call arrives.
	listening.catch(() => {});

	return {
		async call(method: string, params: unknown): Promise<unknown> {
			try {
				await listening;
			} catch (cause) {
				throw new BridgeError(
					method,
					"failed",
					`listening for ${BRIDGE_EVENT_NAME} failed: ${describe(cause)}`,
				);
			}
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
				throw new BridgeError(method, "failed", describe(cause));
			}
		},
		subscribe: hub.subscribe.bind(hub),
	};
}

function describe(cause: unknown): string {
	return cause instanceof Error ? cause.message : String(cause);
}
