import { base64UrlToStandard } from "@/lib/base64";

/**
 * Parser for the pairing QR code and deep link (plan wire protocol):
 *
 *   steno://pair/v1?mac=<uuid>&name=<pct>&fp=<base64url 32 bytes>
 *                  &secret=<base64url 32 bytes>&exp=<unix seconds>
 *
 * `fingerprint` and `secret` come out as standard base64, the encoding the
 * rest of the protocol uses. Hand-parsed because React Native's `URL` and
 * `URLSearchParams` are partial, and so the parser is the same on Node.
 */
export type PairingPayload = {
	macID: string;
	macName: string;
	/** Standard base64 of the 32-byte leaf fingerprint. */
	fingerprint: string;
	/** Standard base64 of the 32-byte pairing secret. */
	secret: string;
	/** Unix seconds. */
	expiresAt: number;
};

export type PairingParseFailure =
	| "not-steno"
	| "version"
	| "missing-field"
	| "bad-encoding"
	| "expired";

export type PairingParseResult =
	| { ok: true; payload: PairingPayload }
	| { ok: false; reason: PairingParseFailure };

const PREFIX = "steno://pair/";
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const FINGERPRINT_BYTES = 32;
const SECRET_BYTES = 32;

function parseQuery(query: string): Map<string, string> | null {
	const params = new Map<string, string>();
	if (query === "") return params;
	for (const pair of query.split("&")) {
		const eq = pair.indexOf("=");
		if (eq <= 0) return null;
		try {
			const key = decodeURIComponent(pair.slice(0, eq));
			const value = decodeURIComponent(pair.slice(eq + 1).replace(/\+/g, " "));
			if (params.has(key)) return null;
			params.set(key, value);
		} catch {
			return null;
		}
	}
	return params;
}

export function parsePairingPayload(
	text: string,
	now: Date,
): PairingParseResult {
	const trimmed = text.trim();
	if (!trimmed.toLowerCase().startsWith(PREFIX)) {
		return { ok: false, reason: "not-steno" };
	}
	const rest = trimmed.slice(PREFIX.length);
	const question = rest.indexOf("?");
	const version = question === -1 ? rest : rest.slice(0, question);
	if (version !== "v1") return { ok: false, reason: "version" };

	const params = parseQuery(question === -1 ? "" : rest.slice(question + 1));
	if (params === null) return { ok: false, reason: "bad-encoding" };

	const mac = params.get("mac");
	const name = params.get("name");
	const fp = params.get("fp");
	const secret = params.get("secret");
	const exp = params.get("exp");
	if (
		mac === undefined ||
		name === undefined ||
		fp === undefined ||
		secret === undefined ||
		exp === undefined
	) {
		return { ok: false, reason: "missing-field" };
	}

	const fingerprint = base64UrlToStandard(fp, FINGERPRINT_BYTES);
	const secretStandard = base64UrlToStandard(secret, SECRET_BYTES);
	const expiresAt = /^\d{1,12}$/.test(exp) ? Number(exp) : Number.NaN;
	if (
		!UUID.test(mac) ||
		name === "" ||
		fingerprint === null ||
		secretStandard === null ||
		!Number.isInteger(expiresAt)
	) {
		return { ok: false, reason: "bad-encoding" };
	}

	if (expiresAt * 1000 <= now.getTime()) {
		return { ok: false, reason: "expired" };
	}

	return {
		ok: true,
		payload: {
			macID: mac.toLowerCase(),
			macName: name,
			fingerprint,
			secret: secretStandard,
			expiresAt,
		},
	};
}

export function describePairingFailure(reason: PairingParseFailure): string {
	switch (reason) {
		case "not-steno":
			return "That code is not a Steno pairing code.";
		case "version":
			return "This code needs a newer version of the app.";
		case "missing-field":
		case "bad-encoding":
			return "The pairing code is damaged. Show a fresh one on the Mac.";
		case "expired":
			return "The pairing code has expired. Show a fresh one on the Mac.";
	}
}
