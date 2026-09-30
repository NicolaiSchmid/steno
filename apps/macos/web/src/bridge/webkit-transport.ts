import { bridgeReply } from "./contract";
import { BridgeError, type BridgeTransport, SnapshotHub } from "./transport";

/**
 * Production transport. Commands go through the `steno` script message
 * handler (a `WKScriptMessageHandlerWithReply`, so `postMessage` returns a
 * promise) as a `BridgeRequest` envelope and come back as a `BridgeReply`
 * envelope, parsed with the contract's schema; the shapes are pinned by
 * `Sources/StenoBridge/BridgeEnvelope.swift` and `fixtures/bridge/envelope.*`. Snapshots arrive when the host
 * evaluates `window.steno.emit(topic, payload)`, one call per coalesced
 * change.
 */

interface WebKitMessageHandler {
	postMessage(message: unknown): Promise<unknown>;
}

interface WebKitNamespace {
	messageHandlers?: Record<string, WebKitMessageHandler | undefined>;
}

export interface StenoHostApi {
	emit(topic: string, payload: unknown): void;
}

declare global {
	interface Window {
		webkit?: WebKitNamespace;
		steno?: StenoHostApi;
	}
}

export function hasWebKitBridge(target: Window = window): boolean {
	return Boolean(target.webkit?.messageHandlers?.steno);
}

let nextRequestID = 0;

export function createWebKitTransport(
	target: Window = window,
): BridgeTransport {
	const handler = target.webkit?.messageHandlers?.steno;
	if (!handler) {
		throw new Error("The steno message handler is not installed");
	}
	const hub = new SnapshotHub();
	target.steno = {
		emit(topic, payload) {
			hub.emit(topic, payload);
		},
	};

	return {
		async call(method: string, params: unknown): Promise<unknown> {
			nextRequestID += 1;
			// The `BridgeRequest` shape; the client has already typed method and params.
			const request = {
				id: `req-${nextRequestID}`,
				method,
				params: params ?? null,
			};
			let reply: unknown;
			try {
				reply = await handler.postMessage(request);
			} catch (cause) {
				const text = cause instanceof Error ? cause.message : String(cause);
				throw new BridgeError(method, "failed", text);
			}
			const parsed = bridgeReply.safeParse(reply);
			if (!parsed.success) {
				throw new BridgeError(
					method,
					"failed",
					`${method} returned a malformed reply`,
				);
			}
			if (parsed.data.id !== request.id) {
				throw new BridgeError(
					method,
					"failed",
					`${method} reply id ${parsed.data.id} does not match ${request.id}`,
				);
			}
			if (parsed.data.error) {
				throw new BridgeError(
					method,
					parsed.data.error.code,
					parsed.data.error.message,
				);
			}
			return parsed.data.result;
		},
		subscribe: hub.subscribe.bind(hub),
	};
}
