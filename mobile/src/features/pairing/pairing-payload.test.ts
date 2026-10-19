import { describe, expect, it } from "vitest";

import { parsePairingPayload } from "./pairing-payload";

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

	const wrong = (length: number) =>
		Buffer.alloc(length, 0xcd).toString("base64url");
	const inSeconds = (delta: number) =>
		String(Math.floor(now.getTime() / 1000) + delta);

	it.each([
		["https://example.com", "not-steno"],
		["steno://record", "not-steno"],
		[url({}, "v2"), "version"],
		["steno://pair/", "version"],
		...["mac", "name", "fp", "secret", "exp"].map((field) => [
			url({ [field]: undefined }),
			"missing-field",
		]),
		[url({ fp: "not*base64" }), "bad-encoding"],
		[url({ fp: "AAAA" }), "bad-encoding"],
		// Standard-alphabet characters and padding never belong in the QR.
		[url({ fp: FP }), "bad-encoding"],
		[url({ fp: `${urlSafe(FP)}=` }), "bad-encoding"],
		...[31, 33, 64].flatMap((length) => [
			[url({ fp: wrong(length) }), "bad-encoding"],
			[url({ secret: wrong(length) }), "bad-encoding"],
		]),
		[url({ mac: "not-a-uuid" }), "bad-encoding"],
		[url({ name: "" }), "bad-encoding"],
		[url({ name: "%E0%A4%A" }), "bad-encoding"],
		[url({ exp: "soon" }), "bad-encoding"],
		[url({ exp: "1790000000000" }), "bad-encoding"],
		[url({ exp: "-5" }), "bad-encoding"],
		// The query is strict: a repeated or key-less parameter is damaged.
		[`${url()}&mac=${MAC}`, "bad-encoding"],
		[`${url()}&stray`, "bad-encoding"],
		[url({ exp: inSeconds(0) }), "expired"],
	])("rejects %s as %s", (text, reason) => {
		expect(parsePairingPayload(text, now)).toEqual({ ok: false, reason });
	});

	it.each([
		["padded with whitespace", `  ${url()}\n`, now],
		["expiring in a second", url({ exp: inSeconds(1) }), now],
		[
			"expiring now, a second ago",
			url({ exp: inSeconds(0) }),
			new Date(now.getTime() - 1000),
		],
	])("accepts a code %s", (_, text, at) => {
		expect(parsePairingPayload(text, at).ok).toBe(true);
	});
});
