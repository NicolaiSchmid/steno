import { stenoLink } from "./native-module";
import type { PinnedResponse } from "./StenoLink.types";

/**
 * The JS face of `PinnedClient.swift`: one small JSON request over the
 * pinned foreground session, and the mapping of its outcome to a
 * `HandoverError`. Every protocol call (pairing and recording alike) goes
 * through here; nothing here knows about the queue or the pairing.
 */
export type MacEndpoint = { origin: string; fingerprint: string };

const REQUEST_TIMEOUT_MS = 10_000;

export type HandoverFailure =
	| "unreachable"
	| "unauthorized"
	| "forbidden"
	| "not-found"
	| "protocol"
	| "server";

export class HandoverError extends Error {
	constructor(
		readonly kind: HandoverFailure,
		readonly status: number | null,
		message: string,
	) {
		super(message);
		this.name = "HandoverError";
	}
}

/**
 * Runs a pinned request; transport failures (including a rejected pin)
 * become `unreachable`. The module adds `Content-Type: application/json`
 * to bodies.
 */
export async function pinnedRequest(
	endpoint: MacEndpoint,
	method: "GET" | "POST" | "PUT" | "DELETE",
	path: string,
	options: { headers?: Record<string, string>; body?: unknown } = {},
): Promise<PinnedResponse> {
	try {
		return await stenoLink().request({
			url: `${endpoint.origin}${path}`,
			method,
			headers: { Accept: "application/json", ...options.headers },
			...(options.body !== undefined
				? { body: JSON.stringify(options.body) }
				: {}),
			fingerprint: endpoint.fingerprint,
			timeoutMs: REQUEST_TIMEOUT_MS,
		});
	} catch (error) {
		throw new HandoverError(
			"unreachable",
			null,
			error instanceof Error ? error.message : String(error),
		);
	}
}

export function failureFor(response: PinnedResponse): HandoverError | null {
	if (response.status < 400) return null;
	if (response.status === 401) {
		return new HandoverError(
			"unauthorized",
			401,
			"The Mac no longer knows this phone",
		);
	}
	if (response.status === 403) {
		return new HandoverError(
			"forbidden",
			403,
			"The Mac rejected the pairing code",
		);
	}
	if (response.status === 404) {
		return new HandoverError("not-found", 404, "The Mac has no such recording");
	}
	return new HandoverError(
		"server",
		response.status,
		`The Mac answered ${response.status}`,
	);
}
