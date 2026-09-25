import {
	authorizationHeader,
	decodeHello,
	decodePairResponse,
	encodeJSON,
	type Hello,
	type PairRequest,
	type PairResponse,
	type PinnedResponse,
	PROTOCOL_VERSION,
	paths,
	type ResolvedMac,
	stenoLink,
} from "@modules/steno-link";

/**
 * The three pairing calls over the pinned foreground session: hello (probe),
 * pair (spend the QR secret for a token) and unpair. Every call needs the
 * Mac's origin and fingerprint; nothing here touches the queue.
 */
export type MacEndpoint = { origin: string; fingerprint: string };

export const REQUEST_TIMEOUT_MS = 10_000;

export function macOrigin(resolved: ResolvedMac): string {
	return `https://${resolved.host}:${resolved.port}`;
}

export type HandoverFailure =
	| "unreachable"
	| "unauthorized"
	| "forbidden"
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

/** Runs a pinned request; transport failures (including a failed pin) become `unreachable`. */
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
			headers: {
				Accept: "application/json",
				...(options.body !== undefined
					? { "Content-Type": "application/json" }
					: {}),
				...options.headers,
			},
			...(options.body !== undefined ? { body: encodeJSON(options.body) } : {}),
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
	return new HandoverError(
		"server",
		response.status,
		`The Mac answered ${response.status}`,
	);
}

export async function hello(endpoint: MacEndpoint): Promise<Hello> {
	const response = await pinnedRequest(endpoint, "GET", paths.hello());
	const failure = failureFor(response);
	if (failure) throw failure;
	const decoded = decodeHello(response.body);
	if (!decoded.ok) {
		throw new HandoverError("protocol", response.status, decoded.reason);
	}
	if (decoded.value.protocol !== PROTOCOL_VERSION) {
		throw new HandoverError(
			"protocol",
			response.status,
			`Mac speaks protocol ${decoded.value.protocol}, phone speaks ${PROTOCOL_VERSION}`,
		);
	}
	return decoded.value;
}

export async function pair(
	endpoint: MacEndpoint,
	secret: string,
	request: PairRequest,
): Promise<PairResponse> {
	const response = await pinnedRequest(endpoint, "POST", paths.pair(), {
		headers: authorizationHeader("Pairing", secret),
		body: request,
	});
	const failure = failureFor(response);
	if (failure) throw failure;
	const decoded = decodePairResponse(response.body);
	if (!decoded.ok) {
		throw new HandoverError("protocol", response.status, decoded.reason);
	}
	return decoded.value;
}

export async function unpair(
	endpoint: MacEndpoint,
	token: string,
): Promise<void> {
	const response = await pinnedRequest(endpoint, "DELETE", paths.pairing(), {
		headers: authorizationHeader("Bearer", token),
	});
	const failure = failureFor(response);
	// 401 means the Mac already forgot us; that is the goal.
	if (failure && failure.kind !== "unauthorized") throw failure;
}
