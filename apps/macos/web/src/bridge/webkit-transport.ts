import { BridgeError, type BridgeTransport, SnapshotHub } from "./transport";

/**
 * Production transport. Commands go through the `steno` script message
 * handler (a `WKScriptMessageHandlerWithReply`, so `postMessage` returns a
 * promise). Snapshots arrive when the host evaluates
 * `window.steno.emit(topic, payload)`, one call per coalesced change.
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

/** The shape every command message has on the wire. */
export interface BridgeCallMessage {
	method: string;
	params: unknown;
}

interface HostErrorReply {
	error: { code?: string; message?: string };
}

function isHostErrorReply(value: unknown): value is HostErrorReply {
	return (
		typeof value === "object" &&
		value !== null &&
		"error" in value &&
		typeof (value as HostErrorReply).error === "object"
	);
}

export function hasWebKitBridge(target: Window = window): boolean {
	return Boolean(target.webkit?.messageHandlers?.steno);
}

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
		async call<TParams, TReply>(
			method: string,
			params: TParams,
		): Promise<TReply> {
			const message: BridgeCallMessage = { method, params };
			let reply: unknown;
			try {
				reply = await handler.postMessage(message);
			} catch (cause) {
				const text = cause instanceof Error ? cause.message : String(cause);
				throw new BridgeError(method, "host", text);
			}
			if (isHostErrorReply(reply)) {
				throw new BridgeError(
					method,
					reply.error.code ?? "unknown",
					reply.error.message ?? `${method} failed`,
				);
			}
			return reply as TReply;
		},
		subscribe: hub.subscribe.bind(hub),
	};
}
