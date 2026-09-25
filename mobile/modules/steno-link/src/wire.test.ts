import { describe, expect, it } from "vitest";

import {
	authorizationHeader,
	CHUNK_HASH_HEADER,
	decodeCompleteResponse,
	decodeHello,
	decodePairResponse,
	decodeRecordingStatus,
	macOrigin,
	PROTOCOL_VERSION,
	paths,
	SERVICE_TYPE,
} from "./wire";

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
