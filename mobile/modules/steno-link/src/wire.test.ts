import { describe, expect, it } from "vitest";

import {
	type AudioFormat,
	authorizationHeader,
	CHUNK_HASH_HEADER,
	type CompleteResponse,
	decodeCompleteResponse,
	decodeHello,
	decodePairResponse,
	decodeRecordingStatus,
	type Hello,
	macOrigin,
	type PairRequest,
	type PairResponse,
	PROTOCOL_VERSION,
	paths,
	RECORDING_STATES,
	type RecordingMetadata,
	type RecordingStatus,
	SERVICE_TYPE,
} from "./wire";

/**
 * The Swift side's field names (core-foundation plan, `RecordingMetadata`
 * and the handover wire types), sorted as `StenoJSON` (`.sortedKeys`) emits
 * them. `Record<keyof T, null>` makes the TypeScript type and this list move
 * together.
 */
const swiftFields = <T extends object>(fields: Record<keyof T, null>) =>
	Object.keys(fields).sort();

const SWIFT = {
	Hello: swiftFields<Hello>({ macID: null, protocol: null }),
	PairRequest: swiftFields<PairRequest>({ deviceID: null, deviceName: null }),
	PairResponse: swiftFields<PairResponse>({
		token: null,
		macID: null,
		macName: null,
	}),
	RecordingMetadata: swiftFields<RecordingMetadata>({
		recordingID: null,
		startedAt: null,
		durationSeconds: null,
		byteCount: null,
		sha256: null,
		chunkSize: null,
		format: null,
		deviceName: null,
	}),
	RecordingStatus: swiftFields<RecordingStatus>({
		state: null,
		receivedChunks: null,
	}),
	CompleteResponse: swiftFields<CompleteResponse>({ meetingID: null }),
};

/** Bodies as the Mac's `StenoJSON` encoder writes them: sorted keys, ISO 8601 with fractions, base64 Data. */
function stenoJSON(value: object): string {
	return JSON.stringify(
		value,
		Object.keys(value).sort((a, b) => a.localeCompare(b)),
	);
}

describe("wire constants", () => {
	it("pins protocol v1 and the service type", () => {
		expect(PROTOCOL_VERSION).toBe(1);
		expect(SERVICE_TYPE).toBe("_steno._tcp");
		expect(CHUNK_HASH_HEADER).toBe("X-Steno-Chunk-SHA256");
	});

	it("builds the v1 paths and escapes the recording id", () => {
		expect(paths.hello).toBe("/v1/hello");
		expect(paths.pair).toBe("/v1/pair");
		expect(paths.pairing).toBe("/v1/pairing");
		expect(paths.recording("a b")).toBe("/v1/recordings/a%20b");
		expect(paths.chunk("id", 3)).toBe("/v1/recordings/id/chunks/3");
		expect(paths.complete("id")).toBe("/v1/recordings/id/complete");
	});

	it("builds the origin from IPv4, bracketed IPv6 and .local hosts as-is", () => {
		expect(macOrigin({ host: "192.168.1.20", port: 51234 })).toBe(
			"https://192.168.1.20:51234",
		);
		expect(macOrigin({ host: "[fe80::1]", port: 1 })).toBe(
			"https://[fe80::1]:1",
		);
		expect(macOrigin({ host: "studio.local", port: 8 })).toBe(
			"https://studio.local:8",
		);
	});

	it("formats the two authorization schemes", () => {
		expect(authorizationHeader("Pairing", "s3")).toEqual({
			Authorization: "Pairing s3",
		});
		expect(authorizationHeader("Bearer", "t0k")).toEqual({
			Authorization: "Bearer t0k",
		});
	});
});

describe("responses", () => {
	it("decodes hello and rejects a missing field or a wrong type", () => {
		expect(decodeHello('{"macID":"m","protocol":1}')).toEqual({
			ok: true,
			value: { macID: "m", protocol: 1 },
		});
		expect(decodeHello('{"macID":"m"}')).toEqual({
			ok: false,
			reason: "protocol must be a number",
		});
		expect(decodeHello('{"macID":1,"protocol":1}')).toEqual({
			ok: false,
			reason: "macID must be a string",
		});
	});

	it("rejects non-JSON and non-object bodies", () => {
		expect(decodeHello("{")).toEqual({ ok: false, reason: "invalid JSON" });
		expect(decodeHello("[]")).toEqual({ ok: false, reason: "not an object" });
		expect(decodeHello("null")).toEqual({ ok: false, reason: "not an object" });
	});

	it("decodes the pair response", () => {
		const value = { token: "t", macID: "m", macName: "Mac" };
		expect(decodePairResponse(JSON.stringify(value))).toEqual({
			ok: true,
			value,
		});
		expect(decodePairResponse('{"token":1}').ok).toBe(false);
	});

	it("decodes recording status and rejects unknown states or bad chunk lists", () => {
		expect(
			decodeRecordingStatus('{"state":"receiving","receivedChunks":[0,2]}'),
		).toEqual({
			ok: true,
			value: { state: "receiving", receivedChunks: [0, 2] },
		});
		expect(
			decodeRecordingStatus('{"state":"paused","receivedChunks":[]}'),
		).toEqual({ ok: false, reason: "unknown state paused" });
		expect(
			decodeRecordingStatus('{"state":"complete","receivedChunks":[0.5]}'),
		).toEqual({
			ok: false,
			reason: "receivedChunks must be an integer array",
		});
	});

	it("decodes the complete response", () => {
		expect(decodeCompleteResponse('{"meetingID":"x"}')).toEqual({
			ok: true,
			value: { meetingID: "x" },
		});
		expect(decodeCompleteResponse("{}").ok).toBe(false);
	});
});

describe("round trips against the Swift names", () => {
	const MAC = "0f8fad5b-d9cb-469f-a165-70867728950e";

	it("Hello, PairResponse, RecordingStatus and CompleteResponse decode from StenoJSON bodies unchanged", () => {
		const hello: Hello = { macID: MAC, protocol: PROTOCOL_VERSION };
		expect(decodeHello(stenoJSON(hello))).toEqual({ ok: true, value: hello });
		expect(Object.keys(hello).sort()).toEqual(SWIFT.Hello);

		const paired: PairResponse = { token: "t", macID: MAC, macName: "Studio" };
		expect(decodePairResponse(stenoJSON(paired))).toEqual({
			ok: true,
			value: paired,
		});
		expect(Object.keys(paired).sort()).toEqual(SWIFT.PairResponse);

		for (const state of RECORDING_STATES) {
			const status: RecordingStatus = { state, receivedChunks: [0, 1, 5] };
			expect(decodeRecordingStatus(stenoJSON(status))).toEqual({
				ok: true,
				value: status,
			});
			expect(Object.keys(status).sort()).toEqual(SWIFT.RecordingStatus);
		}

		const done: CompleteResponse = { meetingID: MAC };
		expect(decodeCompleteResponse(stenoJSON(done))).toEqual({
			ok: true,
			value: done,
		});
		expect(Object.keys(done).sort()).toEqual(SWIFT.CompleteResponse);
	});

	it("tolerates extra fields a newer Mac adds and the four recording states", () => {
		expect(
			decodeHello(`{"macID":"${MAC}","protocol":1,"macName":"later"}`).ok,
		).toBe(true);
		expect(RECORDING_STATES).toEqual([
			"receiving",
			"verifying",
			"complete",
			"failed",
		]);
	});

	it("PairRequest and RecordingMetadata encode with the Swift field names and value shapes", () => {
		const request: PairRequest = { deviceID: MAC, deviceName: "iPhone" };
		expect(Object.keys(JSON.parse(stenoJSON(request))).sort()).toEqual(
			SWIFT.PairRequest,
		);

		const format: AudioFormat = "m4aAAC";
		const metadata: RecordingMetadata = {
			recordingID: MAC,
			startedAt: "2026-09-25T09:00:00.000Z",
			durationSeconds: 3600.25,
			byteCount: 28_800_000,
			sha256: Buffer.alloc(32, 1).toString("base64"),
			chunkSize: 16 * 1024 * 1024,
			format,
			deviceName: "iPhone",
		};
		const encoded = JSON.parse(stenoJSON(metadata)) as Record<string, unknown>;
		expect(Object.keys(encoded).sort()).toEqual(SWIFT.RecordingMetadata);
		// Swift: recordingID: UUID, startedAt: Date (ISO 8601 fractional),
		// sha256: Data (base64 of 32 bytes), byteCount: Int64, format: AudioFormat.
		expect(encoded.recordingID).toMatch(
			/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/,
		);
		expect(encoded.startedAt).toMatch(
			/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$/,
		);
		expect(Buffer.from(encoded.sha256 as string, "base64")).toHaveLength(32);
		expect(Number.isSafeInteger(encoded.byteCount)).toBe(true);
		expect(["caf48kFloat32", "m4aAAC", "wav16kInt16"]).toContain(
			encoded.format,
		);
	});
});
