import { BridgeError, type BridgeTransport, SnapshotHub } from "./transport";

/**
 * Production transport. Commands go through the `steno` script message
 * handler (a `WKScriptMessageHandlerWithReply`, so `postMessage` returns a
 * promise) as a `BridgeRequest` envelope and come back as a `BridgeReply`
 * envelope, the shapes `Sources/StenoBridge/BridgeEnvelope.swift` pins and
 * `fixtures/bridge/envelope.*.json` record. Snapshots arrive when the host
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

/** `BridgeRequest`: what every command posts to the host. */
export interface BridgeRequestEnvelope {
	id: string;
	method: string;
	params: unknown;
}

/** `BridgeReply`: exactly one of `result` and `error` is present. */
export interface BridgeReplyEnvelope {
	id: string;
	result?: unknown;
	error?: { code: string; message: string };
}

function isReplyEnvelope(value: unknown): value is BridgeReplyEnvelope {
	if (typeof value !== "object" || value === null) {
		return false;
	}
	const reply = value as Record<string, unknown>;
	if (typeof reply.id !== "string") {
		return false;
	}
	if (reply.error === undefined) {
		return true;
	}
	return (
		typeof reply.error === "object" &&
		reply.error !== null &&
		typeof (reply.error as Record<string, unknown>).code === "string" &&
		typeof (reply.error as Record<string, unknown>).message === "string"
	);
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
		async call<TParams, TReply>(
			method: string,
			params: TParams,
		): Promise<TReply> {
			nextRequestID += 1;
			const request: BridgeRequestEnvelope = {
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
			if (!isReplyEnvelope(reply)) {
				throw new BridgeError(
					method,
					"failed",
					`${method} returned a malformed reply`,
				);
			}
			if (reply.id !== request.id) {
				throw new BridgeError(
					method,
					"failed",
					`${method} reply id ${reply.id} does not match ${request.id}`,
				);
			}
			if (reply.error) {
				throw new BridgeError(method, reply.error.code, reply.error.message);
			}
			return reply.result as TReply;
		},
		subscribe: hub.subscribe.bind(hub),
	};
}
