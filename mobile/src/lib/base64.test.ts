import { describe, expect, it } from "vitest";

import { base64UrlToBase64, base64UrlToStandard, decodeBase64 } from "./base64";

const bytes = (...values: number[]) => Uint8Array.from(values);

describe("decodeBase64", () => {
	it("decodes every padding length", () => {
		for (const input of [
			bytes(),
			bytes(1),
			bytes(1, 2),
			bytes(1, 2, 3),
			bytes(255, 254, 253, 252),
		]) {
			expect(decodeBase64(Buffer.from(input).toString("base64"))).toEqual(
				input,
			);
		}
	});

	it("rejects wrong lengths, alphabets, padding and non-canonical trailing bits", () => {
		expect(decodeBase64("QUJ")).toBeNull();
		expect(decodeBase64("QU-=")).toBeNull();
		expect(decodeBase64("Q===")).toBeNull();
		expect(decodeBase64("QUI=")).toEqual(bytes(65, 66));
		// "QUJ=" spells the same two bytes with dirty low bits.
		expect(decodeBase64("QUJ=")).toBeNull();
	});
});

describe("base64url conversion", () => {
	it("maps the alphabet and restores padding", () => {
		expect(base64UrlToBase64("-_8")).toBe("+/8=");
		expect(base64UrlToBase64("-_8-")).toBe("+/8+");
		expect(base64UrlToBase64("")).toBe("");
		expect(base64UrlToBase64("A")).toBeNull();
		expect(base64UrlToBase64("A+")).toBeNull();
	});

	it("re-encodes to standard base64 and checks the byte length", () => {
		const thirtyTwo = Buffer.alloc(32, 7);
		const url = thirtyTwo.toString("base64url");
		expect(url.endsWith("=")).toBe(false);
		expect(base64UrlToStandard(url, 32)).toBe(thirtyTwo.toString("base64"));
		expect(base64UrlToStandard(url, 31)).toBeNull();
		expect(base64UrlToStandard("*", 32)).toBeNull();
		// Dirty low bits fail even though the alphabet and length are fine.
		expect(base64UrlToStandard("QUJ")).toBeNull();
		expect(base64UrlToStandard("QUI")).toBe("QUI=");
	});
});
