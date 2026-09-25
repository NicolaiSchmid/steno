/**
 * Wire types of the phone-to-Mac handover protocol (v1).
 *
 * Every name here mirrors the Swift type in StenoCore / StenoHandover exactly,
 * encoded with the `StenoJSON` convention: camelCase keys, ISO 8601 dates with
 * fractional seconds, `Data` fields as standard base64 strings. Only the QR
 * pairing URL uses base64url, because it travels in a query string; see
 * `src/features/pairing/pairing-payload.ts`.
 *
 * Pure module: no native imports, unit-tested by `wire.test.ts`.
 */

export const PROTOCOL_VERSION = 1;
export const SERVICE_TYPE = "_steno._tcp";

/** Raw values of core's `AudioFormat`. The phone only ever sends `m4aAAC`. */
export type AudioFormat = "caf48kFloat32" | "m4aAAC" | "wav16kInt16";
export const AUDIO_FORMATS: readonly AudioFormat[] = [
	"caf48kFloat32",
	"m4aAAC",
	"wav16kInt16",
];

/** `GET /v1/hello` response, the reachability probe. */
export type Hello = { macID: string; protocol: number };

/** `POST /v1/pair` request, sent with `Authorization: Pairing <secret>`. */
export type PairRequest = { deviceID: string; deviceName: string };

/** `POST /v1/pair` response. `token` is the bearer for every later call. */
export type PairResponse = { token: string; macID: string; macName: string };

/** `PUT /v1/recordings/{id}` body; core's `RecordingMetadata`. */
export type RecordingMetadata = {
	recordingID: string;
	startedAt: string;
	durationSeconds: number;
	byteCount: number;
	sha256: string;
	chunkSize: number;
	format: AudioFormat;
	deviceName: string;
};

export type RecordingState = "receiving" | "verifying" | "complete" | "failed";
export const RECORDING_STATES: readonly RecordingState[] = [
	"receiving",
	"verifying",
	"complete",
	"failed",
];

/** `GET /v1/recordings/{id}` and the announce response, used to resume. */
export type RecordingStatus = {
	state: RecordingState;
	receivedChunks: number[];
};

/** `POST /v1/recordings/{id}/complete` 200 body. */
export type CompleteResponse = { meetingID: string };

/** Request header carrying the SHA-256 (standard base64) of one chunk body. */
export const CHUNK_HASH_HEADER = "X-Steno-Chunk-SHA256";

/** Paths, relative to the resolved Mac origin. */
export const paths = {
	hello: () => "/v1/hello",
	pair: () => "/v1/pair",
	pairing: () => "/v1/pairing",
	recording: (recordingID: string) =>
		`/v1/recordings/${encodeURIComponent(recordingID)}`,
	chunk: (recordingID: string, index: number) =>
		`/v1/recordings/${encodeURIComponent(recordingID)}/chunks/${index}`,
	complete: (recordingID: string) =>
		`/v1/recordings/${encodeURIComponent(recordingID)}/complete`,
} as const;

export function authorizationHeader(
	scheme: "Pairing" | "Bearer",
	credential: string,
): Record<string, string> {
	return { Authorization: `${scheme} ${credential}` };
}

export type Decoded<T> = { ok: true; value: T } | { ok: false; reason: string };

/** JSON body encoding; camelCase falls out of the type literals above. */
export function encodeJSON(value: unknown): string {
	return JSON.stringify(value);
}

type Shape = Record<string, "string" | "number" | "number[]">;

function decodeShape<T>(text: string, shape: Shape): Decoded<T> {
	let parsed: unknown;
	try {
		parsed = JSON.parse(text);
	} catch {
		return { ok: false, reason: "invalid JSON" };
	}
	if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
		return { ok: false, reason: "not an object" };
	}
	const record = parsed as Record<string, unknown>;
	for (const [key, kind] of Object.entries(shape)) {
		const value = record[key];
		if (kind === "number[]") {
			if (
				!Array.isArray(value) ||
				!value.every((n) => typeof n === "number" && Number.isInteger(n))
			) {
				return { ok: false, reason: `${key} must be an integer array` };
			}
		} else if (typeof value !== kind) {
			return { ok: false, reason: `${key} must be a ${kind}` };
		} else if (kind === "number" && !Number.isFinite(value)) {
			return { ok: false, reason: `${key} must be finite` };
		}
	}
	return { ok: true, value: record as T };
}

export function decodeHello(text: string): Decoded<Hello> {
	return decodeShape<Hello>(text, { macID: "string", protocol: "number" });
}

export function decodePairResponse(text: string): Decoded<PairResponse> {
	return decodeShape<PairResponse>(text, {
		token: "string",
		macID: "string",
		macName: "string",
	});
}

export function decodeRecordingStatus(text: string): Decoded<RecordingStatus> {
	const decoded = decodeShape<RecordingStatus>(text, {
		state: "string",
		receivedChunks: "number[]",
	});
	if (!decoded.ok) return decoded;
	if (!RECORDING_STATES.includes(decoded.value.state)) {
		return { ok: false, reason: `unknown state ${decoded.value.state}` };
	}
	return decoded;
}

export function decodeCompleteResponse(
	text: string,
): Decoded<CompleteResponse> {
	return decodeShape<CompleteResponse>(text, { meetingID: "string" });
}

export function decodeRecordingMetadata(
	text: string,
): Decoded<RecordingMetadata> {
	const decoded = decodeShape<RecordingMetadata>(text, {
		recordingID: "string",
		startedAt: "string",
		durationSeconds: "number",
		byteCount: "number",
		sha256: "string",
		chunkSize: "number",
		format: "string",
		deviceName: "string",
	});
	if (!decoded.ok) return decoded;
	if (!AUDIO_FORMATS.includes(decoded.value.format)) {
		return { ok: false, reason: `unknown format ${decoded.value.format}` };
	}
	return decoded;
}
