import type { ResolvedMac } from "@modules/steno-link";
import {
	type Hello,
	macOrigin,
	type PairRequest,
	type PairResponse,
} from "@modules/steno-link/src/wire";
import type { MacEndpoint } from "./pairing-client";
import type { PairingPayload } from "./pairing-payload";
import type { DeviceIdentity, Pairing } from "./pairing-store";

/**
 * The pairing sequence after a QR code parsed (plan P5): find the Mac the
 * code names on the local network, confirm it is that Mac over the pinned
 * channel, spend the single-use secret for a token. Dependencies are
 * injected so the sequence is unit-tested without a network.
 */
export type PairingDependencies = {
	locate(macID: string): Promise<ResolvedMac>;
	hello(endpoint: MacEndpoint): Promise<Hello>;
	pair(
		endpoint: MacEndpoint,
		secret: string,
		request: PairRequest,
	): Promise<PairResponse>;
	device(): Promise<DeviceIdentity>;
	now(): Date;
};

export class PairingMismatchError extends Error {
	constructor(step: "hello" | "pair") {
		super(
			step === "hello"
				? "The Mac at that address is not the one on the code"
				: "The Mac answered with a different identity",
		);
		this.name = "PairingMismatchError";
	}
}

export async function performPairing(
	payload: PairingPayload,
	deps: PairingDependencies,
): Promise<Pairing> {
	const endpoint: MacEndpoint = {
		origin: macOrigin(await deps.locate(payload.macID)),
		fingerprint: payload.fingerprint,
	};

	const greeting = await deps.hello(endpoint);
	if (greeting.macID.toLowerCase() !== payload.macID.toLowerCase()) {
		throw new PairingMismatchError("hello");
	}

	const device = await deps.device();
	const response = await deps.pair(endpoint, payload.secret, {
		deviceID: device.deviceID,
		deviceName: device.deviceName,
	});
	if (response.macID.toLowerCase() !== payload.macID.toLowerCase()) {
		throw new PairingMismatchError("pair");
	}

	return {
		mac: {
			macID: payload.macID,
			macName: response.macName || payload.macName,
			fingerprint: payload.fingerprint,
			pairedAt: deps.now().toISOString(),
		},
		token: response.token,
	};
}
