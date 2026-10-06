import {
	type Hello,
	macOrigin,
	type PairRequest,
	type PairResponse,
	type ResolvedMac,
} from "@modules/steno-link";
import type { MacEndpoint } from "@modules/steno-link/native";

import {
	type QueueIndex,
	resetForUpload,
	unpairPending,
} from "@/features/queue/queue-index";
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

/** What a pairing changes on the phone, injected so the order is tested. */
export type PairingCommitDependencies = {
	replace(pairing: Pairing): Promise<void>;
	/** Cancels every chunk still in the background session. */
	cancelAllUploads(): Promise<void>;
	update(transform: (index: QueueIndex) => QueueIndex): Promise<unknown>;
};

/**
 * Makes `pairing` the phone's; rejects when the save fails. `replace` first:
 * from the call on, the upload loop starts nothing under the old pairing.
 * Chunks already in flight went out under it: the same Mac answers them 401,
 * another Mac would take them. Cancel them; the retry backoff re-queues them.
 * Then anything the old Mac revoked is eligible for the new one.
 */
export async function commitPairing(
	deps: PairingCommitDependencies,
	pairing: Pairing,
): Promise<void> {
	await Promise.all([deps.replace(pairing), cancelAll(deps)]);
	await deps.update((index) =>
		index.recordings
			.filter((r) => r.state === "unpaired")
			.reduce((acc, r) => resetForUpload(acc, r.recordingID), index),
	);
}

export type PairingForgetDependencies = Omit<
	PairingCommitDependencies,
	"replace"
> & {
	clear(): Promise<void>;
};

/**
 * Forgets the pairing on the phone, then cancels the chunks still in the
 * background session: the Mac may have been away when asked to revoke the
 * token, and would take them when it comes back. The rows are `unpaired`
 * before the cancel, so the cancelled chunks' failures leave them alone.
 */
export async function forgetPairing(
	deps: PairingForgetDependencies,
): Promise<void> {
	await deps.clear();
	await deps.update(unpairPending);
	await cancelAll(deps);
}

/** A failed cancel is logged: the pairing change goes on without it. */
function cancelAll(deps: Pick<PairingCommitDependencies, "cancelAllUploads">) {
	return deps
		.cancelAllUploads()
		.catch((error) => console.warn("[pairing] cancel failed", error));
}
