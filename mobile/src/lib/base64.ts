/**
 * Strict base64 and base64url decoding without `atob`, so the pairing parser
 * runs identically on Hermes and Node. Wrong alphabet, bad padding or a
 * dangling sextet returns null instead of a best-effort result.
 */

const LOOKUP = new Map<string, number>(
	[..."ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"].map(
		(char, value) => [char, value] as const,
	),
);

/** `-_` alphabet and no padding to `+/` with padding. Null when not base64url. */
export function base64UrlToBase64(text: string): string | null {
	if (!/^[A-Za-z0-9_-]*$/.test(text)) return null;
	const standard = text.replace(/-/g, "+").replace(/_/g, "/");
	const remainder = standard.length % 4;
	if (remainder === 1) return null;
	return remainder === 0 ? standard : standard + "=".repeat(4 - remainder);
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

/**
 * Base64url text as standard base64, or null when it is not canonical base64url
 * of exactly `byteLength` bytes. A decode that passes is canonical, so the
 * padded text is already the standard encoding.
 */
export function base64UrlToStandard(
	text: string,
	byteLength?: number,
): string | null {
	const standard = base64UrlToBase64(text);
	if (standard === null) return null;
	const bytes = decodeBase64(standard);
	if (bytes === null) return null;
	if (byteLength !== undefined && bytes.length !== byteLength) return null;
	return standard;
}
