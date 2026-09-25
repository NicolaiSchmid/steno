/**
 * Standard and URL-safe base64 without relying on `atob`/`btoa`, so the
 * pairing parser and the wire helpers run identically on Hermes and Node.
 * Decoding is strict: wrong alphabet, bad padding or a dangling sextet
 * returns null instead of a best-effort result.
 */

const STANDARD =
	"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const LOOKUP = new Map<string, number>(
	[...STANDARD].map((char, value) => [char, value] as const),
);

/** `-_` alphabet and no padding to `+/` with padding. Null when not base64url. */
export function base64UrlToBase64(text: string): string | null {
	if (!/^[A-Za-z0-9_-]*$/.test(text)) return null;
	const standard = text.replace(/-/g, "+").replace(/_/g, "/");
	const remainder = standard.length % 4;
	if (remainder === 1) return null;
	return remainder === 0 ? standard : standard + "=".repeat(4 - remainder);
}

export function base64ToBase64Url(text: string): string {
	return text.replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function decodeBase64(text: string): Uint8Array | null {
	if (text.length % 4 !== 0) return null;
	const padding = text.endsWith("==") ? 2 : text.endsWith("=") ? 1 : 0;
	const body = text.slice(0, text.length - padding);
	if (!/^[A-Za-z0-9+/]*$/.test(body)) return null;
	const bytes = new Uint8Array((text.length / 4) * 3 - padding);
	let buffer = 0;
	let bits = 0;
	let out = 0;
	for (const char of body) {
		const value = LOOKUP.get(char);
		if (value === undefined) return null;
		buffer = (buffer << 6) | value;
		bits += 6;
		if (bits >= 8) {
			bits -= 8;
			bytes[out++] = (buffer >> bits) & 0xff;
		}
	}
	// Leftover bits must be zero (canonical encoding).
	if (bits > 0 && (buffer & ((1 << bits) - 1)) !== 0) return null;
	return out === bytes.length ? bytes : null;
}

export function encodeBase64(bytes: Uint8Array): string {
	let output = "";
	for (let i = 0; i < bytes.length; i += 3) {
		const a = bytes[i] ?? 0;
		const b = bytes[i + 1];
		const c = bytes[i + 2];
		const triple = (a << 16) | ((b ?? 0) << 8) | (c ?? 0);
		output += STANDARD[(triple >> 18) & 63];
		output += STANDARD[(triple >> 12) & 63];
		output += b === undefined ? "=" : STANDARD[(triple >> 6) & 63];
		output += c === undefined ? "=" : STANDARD[triple & 63];
	}
	return output;
}

/** Decodes base64url text and re-encodes it as standard base64, or null. */
export function base64UrlToStandard(
	text: string,
	byteLength?: number,
): string | null {
	const standard = base64UrlToBase64(text);
	if (standard === null) return null;
	const bytes = decodeBase64(standard);
	if (bytes === null) return null;
	if (byteLength !== undefined && bytes.length !== byteLength) return null;
	return encodeBase64(bytes);
}
