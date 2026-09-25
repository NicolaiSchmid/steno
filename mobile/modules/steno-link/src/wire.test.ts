import { describe, expect, it } from "vitest";

import {
	authorizationHeader,
	CHUNK_HASH_HEADER,
	decodeCompleteResponse,
	decodeHello,
	decodePairResponse,
	decodeRecordingMetadata,
	decodeRecordingStatus,
	encodeJSON,
	PROTOCOL_VERSION,
	paths,
	type RecordingMetadata,
	SERVICE_TYPE,
} from "./wire";

const metadata: RecordingMetadata = {
	recordingID: "0f8fad5b-d9cb-469f-a165-70867728950e",
	startedAt: "2026-09-25T09:30:00.000Z",
	durationSeconds: 3600.25,
	byteCount: 29_000_000,
	sha256: "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=",
	chunkSize: 16 * 1024 * 1024,
	format: "m4aAAC",
	deviceName: "Nicolai's iPhone",
};

describe("wire constants", () => {
	it("pins protocol v1 and the service type", () => {
		expect(PROTOCOL_VERSION).toBe(1);
		expect(SERVICE_TYPE).toBe("_steno._tcp");
		expect(CHUNK_HASH_HEADER).toBe("X-Steno-Chunk-SHA256");
	});

	it("builds the v1 paths and escapes the recording id", () => {
		expect(paths.hello()).toBe("/v1/hello");
		expect(paths.pair()).toBe("/v1/pair");
		expect(paths.pairing()).toBe("/v1/pairing");
		expect(paths.recording("a b")).toBe("/v1/recordings/a%20b");
		expect(paths.chunk("id", 3)).toBe("/v1/recordings/id/chunks/3");
		expect(paths.complete("id")).toBe("/v1/recordings/id/complete");
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

describe("RecordingMetadata", () => {
	it("round-trips with the Swift key names in order", () => {
		const text = encodeJSON(metadata);
		expect(Object.keys(JSON.parse(text))).toEqual([
			"recordingID",
			"startedAt",
			"durationSeconds",
			"byteCount",
			"sha256",
			"chunkSize",
			"format",
			"deviceName",
		]);
		expect(decodeRecordingMetadata(text)).toEqual({
			ok: true,
			value: metadata,
		});
	});

	it("rejects a missing field, a wrong type and an unknown format", () => {
		const { deviceName: _dropped, ...withoutDevice } = metadata;
		expect(decodeRecordingMetadata(encodeJSON(withoutDevice))).toEqual({
			ok: false,
			reason: "deviceName must be a string",
		});
		expect(
			decodeRecordingMetadata(encodeJSON({ ...metadata, byteCount: "29" })),
		).toEqual({ ok: false, reason: "byteCount must be a number" });
		expect(
			decodeRecordingMetadata(encodeJSON({ ...metadata, format: "mp3" })),
		).toEqual({ ok: false, reason: "unknown format mp3" });
	});

	it("rejects non-JSON and non-object bodies", () => {
		expect(decodeRecordingMetadata("{")).toEqual({
			ok: false,
			reason: "invalid JSON",
		});
		expect(decodeRecordingMetadata("[]")).toEqual({
			ok: false,
			reason: "not an object",
		});
		expect(decodeRecordingMetadata("null")).toEqual({
			ok: false,
			reason: "not an object",
		});
	});
});

describe("responses", () => {
	it("decodes hello", () => {
		expect(decodeHello('{"macID":"m","protocol":1}')).toEqual({
			ok: true,
			value: { macID: "m", protocol: 1 },
		});
		expect(decodeHello('{"macID":"m"}').ok).toBe(false);
	});

	it("decodes the pair response", () => {
		const value = { token: "t", macID: "m", macName: "Mac" };
		expect(decodePairResponse(encodeJSON(value))).toEqual({ ok: true, value });
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
