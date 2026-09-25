import { describe, expect, it, vi } from "vitest";

import {
	type PairingDependencies,
	PairingMismatchError,
	performPairing,
} from "./pairing-flow";
import type { PairingPayload } from "./pairing-payload";

const payload: PairingPayload = {
	macID: "0f8fad5b-d9cb-469f-a165-70867728950e",
	macName: "Studio",
	fingerprint: Buffer.alloc(32, 1).toString("base64"),
	secret: Buffer.alloc(32, 2).toString("base64"),
	expiresAt: 1_790_000_000,
};

function deps(
	overrides: Partial<PairingDependencies> = {},
): PairingDependencies {
	return {
		locate: vi.fn(async () => ({ host: "192.168.1.20", port: 51234 })),
		hello: vi.fn(async () => ({
			macID: payload.macID.toUpperCase(),
			protocol: 1,
		})),
		pair: vi.fn(async () => ({
			token: "tok",
			macID: payload.macID,
			macName: "Studio (renamed)",
		})),
		device: vi.fn(async () => ({ deviceID: "dev-1", deviceName: "iPhone" })),
		now: () => new Date("2026-09-25T10:00:00.000Z"),
		...overrides,
	};
}

describe("performPairing", () => {
	it("locates, greets, pairs with the pinned fingerprint and returns the pairing", async () => {
		const d = deps();
		const outcome = await performPairing(payload, d);
		const endpoint = {
			origin: "https://192.168.1.20:51234",
			fingerprint: payload.fingerprint,
		};
		expect(d.locate).toHaveBeenCalledWith(payload.macID);
		expect(d.hello).toHaveBeenCalledWith(endpoint);
		expect(d.pair).toHaveBeenCalledWith(endpoint, payload.secret, {
			deviceID: "dev-1",
			deviceName: "iPhone",
		});
		expect(outcome).toEqual({
			origin: "https://192.168.1.20:51234",
			token: "tok",
			mac: {
				macID: payload.macID,
				macName: "Studio (renamed)",
				fingerprint: payload.fingerprint,
				pairedAt: "2026-09-25T10:00:00.000Z",
			},
		});
	});

	it("brackets IPv6 hosts as returned by the module", async () => {
		const d = deps({ locate: async () => ({ host: "[fe80::1]", port: 1 }) });
		const outcome = await performPairing(payload, d);
		expect(outcome.origin).toBe("https://[fe80::1]:1");
	});

	it("stops before spending the secret when hello names another Mac", async () => {
		const d = deps({ hello: async () => ({ macID: "other", protocol: 1 }) });
		await expect(performPairing(payload, d)).rejects.toBeInstanceOf(
			PairingMismatchError,
		);
		expect(d.pair).not.toHaveBeenCalled();
	});

	it("rejects a pair response from a different identity", async () => {
		const d = deps({
			pair: async () => ({ token: "t", macID: "other", macName: "X" }),
		});
		await expect(performPairing(payload, d)).rejects.toThrow(
			/different identity/,
		);
	});

	it("falls back to the code's name when the Mac sends none", async () => {
		const d = deps({
			pair: async () => ({ token: "t", macID: payload.macID, macName: "" }),
		});
		expect((await performPairing(payload, d)).mac.macName).toBe("Studio");
	});

	it("propagates a failed locate", async () => {
		const d = deps({
			locate: async () => Promise.reject(new Error("not found")),
		});
		await expect(performPairing(payload, d)).rejects.toThrow("not found");
		expect(d.hello).not.toHaveBeenCalled();
	});
});
