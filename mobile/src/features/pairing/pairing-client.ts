import {
	authorizationHeader,
	decodeHello,
	decodePairResponse,
	type Hello,
	type PairRequest,
	type PairResponse,
	PROTOCOL_VERSION,
	paths,
} from "@modules/steno-link";
import {
	failureFor,
	HandoverError,
	type MacEndpoint,
	pinnedRequest,
} from "@modules/steno-link/native";

/**
 * The three pairing calls over the pinned transport: hello (probe), pair
 * (spend the QR secret for a token) and unpair. Every call needs the Mac's
 * origin and fingerprint; nothing here touches the queue.
 */
export async function hello(endpoint: MacEndpoint): Promise<Hello> {
	const response = await pinnedRequest(endpoint, "GET", paths.hello);
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
	const response = await pinnedRequest(endpoint, "POST", paths.pair, {
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
	const response = await pinnedRequest(endpoint, "DELETE", paths.pairing, {
		headers: authorizationHeader("Bearer", token),
	});
	const failure = failureFor(response);
	// 401 means the Mac already forgot us; that is the goal.
	if (failure && failure.kind !== "unauthorized") throw failure;
}
