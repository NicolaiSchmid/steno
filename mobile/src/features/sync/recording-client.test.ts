import type { PinnedRequest, UploadSpec } from "@modules/steno-link";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { HandoverError } from "@/features/pairing/pairing-client";
import {
	addRecording,
	EMPTY_INDEX,
	type QueuedRecording,
} from "@/features/queue/queue-index";
import {
	announce,
	complete,
	metadataFor,
	type Session,
	startChunkUpload,
	status,
} from "./recording-client";

const link = vi.hoisted(() => ({
	request: vi.fn(),
	startUpload: vi.fn(),
}));
vi.mock("expo", () => ({ requireNativeModule: () => link }));

const session: Session = {
	endpoint: { origin: "https://192.168.1.20:51234", fingerprint: "FP" },
	token: "tok",
};
const SHA = Buffer.alloc(32, 3).toString("base64");
const row = addRecording(EMPTY_INDEX, {
	recordingID: "rec 1",
	fileName: "rec 1.m4a",
	startedAt: "2026-09-25T09:00:00.000Z",
	durationSeconds: 61.5,
	byteCount: 3000,
	sha256: SHA,
	chunkSize: 1024,
}).recordings[0] as QueuedRecording;

/** Field names of core's `RecordingMetadata`, in the order StenoJSON sorts them. */
const SWIFT_METADATA_FIELDS = [
	"byteCount",
	"chunkSize",
	"deviceName",
	"durationSeconds",
	"format",
	"recordingID",
	"sha256",
	"startedAt",
];

function answer(status: number, body: unknown = "") {
	link.request.mockResolvedValueOnce({
		status,
		headers: {},
		body: typeof body === "string" ? body : JSON.stringify(body),
	});
}

function lastRequest(): PinnedRequest {
	return link.request.mock.calls.at(-1)?.[0] as PinnedRequest;
}

beforeEach(() => {
	link.request.mockReset();
	link.startUpload.mockReset();
});

describe("metadataFor", () => {
	it("encodes core's RecordingMetadata by name with m4aAAC and the device name", () => {
		const metadata = metadataFor(row, "iPhone");
		expect(Object.keys(metadata).sort()).toEqual(SWIFT_METADATA_FIELDS);
		expect(metadata).toEqual({
			recordingID: "rec 1",
			startedAt: "2026-09-25T09:00:00.000Z",
			durationSeconds: 61.5,
			byteCount: 3000,
			sha256: SHA,
			chunkSize: 1024,
			format: "m4aAAC",
			deviceName: "iPhone",
		});
		// ISO 8601 with fractional seconds, as StenoJSON's decoder expects.
		expect(metadata.startedAt).toMatch(
			/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$/,
		);
	});

	it("refuses a recording without a hash", () => {
		expect(() => metadataFor({ ...row, sha256: null }, "iPhone")).toThrow(
			/no hash/,
		);
	});
});

describe("announce", () => {
	it("PUTs the metadata as JSON with the bearer to the recording path and returns the status", async () => {
		answer(201, { state: "receiving", receivedChunks: [] });
		const result = await announce(session, metadataFor(row, "iPhone"));
		expect(result).toEqual({ state: "receiving", receivedChunks: [] });
		const request = lastRequest();
		expect(request).toMatchObject({
			url: "https://192.168.1.20:51234/v1/recordings/rec%201",
			method: "PUT",
			headers: { Accept: "application/json", Authorization: "Bearer tok" },
			fingerprint: "FP",
			timeoutMs: 10_000,
		});
		expect(Object.keys(JSON.parse(request.body ?? "")).sort()).toEqual(
			SWIFT_METADATA_FIELDS,
		);
	});

	it("treats 200 (already known) like 201 and resumes from its chunk set", async () => {
		answer(200, { state: "receiving", receivedChunks: [0, 2] });
		expect(
			(await announce(session, metadataFor(row, "iPhone"))).receivedChunks,
		).toEqual([0, 2]);
	});

	it("maps 401 to unauthorized and a bad body to a protocol error", async () => {
		answer(401);
		await expect(
			announce(session, metadataFor(row, "iPhone")),
		).rejects.toMatchObject({ kind: "unauthorized", status: 401 });
		answer(201, { state: "paused", receivedChunks: [] });
		await expect(
			announce(session, metadataFor(row, "iPhone")),
		).rejects.toMatchObject({ kind: "protocol", message: /paused/ });
	});

	it("turns a transport failure (including a failed pin) into unreachable", async () => {
		link.request.mockRejectedValueOnce(
			new Error("The certificate for this server is invalid."),
		);
		const error = await announce(session, metadataFor(row, "iPhone")).catch(
			(e) => e,
		);
		expect(error).toBeInstanceOf(HandoverError);
		expect(error).toMatchObject({
			kind: "unreachable",
			status: null,
			message: /certificate/,
		});
	});
});

describe("status", () => {
	it("GETs the recording path with the bearer and surfaces 404 as not-found", async () => {
		answer(200, { state: "verifying", receivedChunks: [0, 1] });
		expect(await status(session, "rec 1")).toEqual({
			state: "verifying",
			receivedChunks: [0, 1],
		});
		expect(lastRequest()).toMatchObject({
			url: "https://192.168.1.20:51234/v1/recordings/rec%201",
			method: "GET",
			headers: { Authorization: "Bearer tok" },
		});
		expect(lastRequest().body).toBeUndefined();
		answer(404);
		await expect(status(session, "rec 1")).rejects.toMatchObject({
			kind: "not-found",
		});
	});
});

describe("complete", () => {
	it("POSTs to the complete path and decodes the meeting id", async () => {
		answer(200, { meetingID: "m-1" });
		expect(await complete(session, "rec 1")).toEqual({
			kind: "complete",
			meetingID: "m-1",
		});
		expect(lastRequest()).toMatchObject({
			url: "https://192.168.1.20:51234/v1/recordings/rec%201/complete",
			method: "POST",
			headers: { Authorization: "Bearer tok" },
		});
	});

	it("reports 409 and 422 as results, not errors", async () => {
		answer(409);
		expect(await complete(session, "rec 1")).toEqual({
			kind: "missing-chunks",
		});
		answer(422);
		expect(await complete(session, "rec 1")).toEqual({
			kind: "hash-mismatch",
		});
	});

	it("throws on 401, 5xx and an undecodable 200", async () => {
		answer(401);
		await expect(complete(session, "rec 1")).rejects.toMatchObject({
			kind: "unauthorized",
		});
		answer(500);
		await expect(complete(session, "rec 1")).rejects.toMatchObject({
			kind: "server",
			status: 500,
		});
		answer(200, {});
		await expect(complete(session, "rec 1")).rejects.toMatchObject({
			kind: "protocol",
		});
	});
});

describe("startChunkUpload", () => {
	it("hands the background session an UploadSpec with the chunk's slice and the pin", async () => {
		link.startUpload.mockResolvedValueOnce(undefined);
		await startChunkUpload(
			session,
			"rec 1",
			{ index: 2, offset: 2048, length: 952 },
			"file:///docs/queue/rec%201.m4a",
		);
		const spec = link.startUpload.mock.calls[0]?.[0] as UploadSpec;
		expect(spec).toEqual({
			taskID: "rec 1/2",
			url: "https://192.168.1.20:51234/v1/recordings/rec%201/chunks/2",
			headers: { Authorization: "Bearer tok" },
			fingerprint: "FP",
			filePath: "file:///docs/queue/rec%201.m4a",
			offset: 2048,
			length: 952,
		});
	});

	it("propagates the module's rejection", async () => {
		link.startUpload.mockRejectedValueOnce(new Error("ERR_STENO_UPLOAD"));
		await expect(
			startChunkUpload(session, "r", { index: 0, offset: 0, length: 1 }, "f"),
		).rejects.toThrow("ERR_STENO_UPLOAD");
	});
});
