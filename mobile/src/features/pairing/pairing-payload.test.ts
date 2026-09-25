import { describe, expect, it } from "vitest";

import { describePairingFailure, parsePairingPayload } from "./pairing-payload";

const now = new Date("2026-09-25T10:00:00.000Z");
const FP = Buffer.alloc(32, 0xab).toString("base64");
const SECRET = Buffer.alloc(32, 0x12).toString("base64");
const MAC = "0F8FAD5B-D9CB-469F-A165-70867728950E";
const urlSafe = (standard: string) =>
	Buffer.from(standard, "base64").toString("base64url");

function url(overrides: Partial<Record<string, string>> = {}, version = "v1") {
	const params: Record<string, string | undefined> = {
		mac: MAC,
		name: encodeURIComponent("Nicolai's Mac"),
		fp: urlSafe(FP),
		secret: urlSafe(SECRET),
		exp: String(Math.floor(now.getTime() / 1000) + 300),
		...overrides,
	};
	const query = Object.entries(params)
		.filter(([, v]) => v !== undefined)
		.map(([k, v]) => `${k}=${v}`)
		.join("&");
	return `steno://pair/${version}?${query}`;
}

describe("parsePairingPayload", () => {
	it("parses a valid code into standard base64 and a lowercase uuid", () => {
		expect(parsePairingPayload(url(), now)).toEqual({
			ok: true,
			payload: {
				macID: MAC.toLowerCase(),
				macName: "Nicolai's Mac",
				fingerprint: FP,
				secret: SECRET,
				expiresAt: Math.floor(now.getTime() / 1000) + 300,
			},
		});
	});

	it("rejects non-steno text and other versions", () => {
		expect(parsePairingPayload("https://example.com", now)).toEqual({
			ok: false,
			reason: "not-steno",
		});
		expect(parsePairingPayload("steno://record", now)).toEqual({
			ok: false,
			reason: "not-steno",
		});
		expect(parsePairingPayload(url({}, "v2"), now)).toEqual({
			ok: false,
			reason: "version",
		});
		expect(parsePairingPayload("steno://pair/", now)).toEqual({
			ok: false,
			reason: "version",
		});
	});

	it("rejects a missing field", () => {
		for (const field of ["mac", "name", "fp", "secret", "exp"]) {
			expect(parsePairingPayload(url({ [field]: undefined }), now)).toEqual({
				ok: false,
				reason: "missing-field",
			});
		}
	});

	it("rejects bad base64url, wrong lengths, bad uuids and bad expiries", () => {
		expect(parsePairingPayload(url({ fp: "not*base64" }), now).ok).toBe(false);
		expect(parsePairingPayload(url({ fp: "AAAA" }), now)).toEqual({
			ok: false,
			reason: "bad-encoding",
		});
		expect(parsePairingPayload(url({ fp: FP }), now)).toEqual({
			ok: false,
			reason: "bad-encoding",
		});
		expect(parsePairingPayload(url({ secret: "AAAA" }), now).ok).toBe(false);
		expect(parsePairingPayload(url({ mac: "not-a-uuid" }), now)).toEqual({
			ok: false,
			reason: "bad-encoding",
		});
		expect(parsePairingPayload(url({ exp: "soon" }), now)).toEqual({
			ok: false,
			reason: "bad-encoding",
		});
		expect(parsePairingPayload(url({ name: "" }), now)).toEqual({
			ok: false,
			reason: "bad-encoding",
		});
		expect(parsePairingPayload(url({ name: "%E0%A4%A" }), now)).toEqual({
			ok: false,
			reason: "bad-encoding",
		});
	});

	it("rejects an expired code on the injected clock", () => {
		const exp = String(Math.floor(now.getTime() / 1000));
		expect(parsePairingPayload(url({ exp }), now)).toEqual({
			ok: false,
			reason: "expired",
		});
		const stillValid = new Date(now.getTime() - 1000);
		expect(parsePairingPayload(url({ exp }), stillValid).ok).toBe(true);
	});

	it("has a sentence for every failure", () => {
		for (const reason of [
			"not-steno",
			"version",
			"missing-field",
			"bad-encoding",
			"expired",
		] as const) {
			expect(describePairingFailure(reason)).toMatch(/\.$/);
		}
	});
});
