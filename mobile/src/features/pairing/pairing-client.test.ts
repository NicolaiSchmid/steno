import type { PinnedRequest } from "@modules/steno-link";
import {
	failureFor,
	HandoverError,
	type MacEndpoint,
	pinnedRequest,
} from "@modules/steno-link/native";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { hello, pair, unpair } from "./pairing-client";

const link = vi.hoisted(() => ({ request: vi.fn() }));
vi.mock("expo", () => ({ requireNativeModule: () => link }));

const endpoint: MacEndpoint = {
	origin: "https://[fe80::1]:4433",
	fingerprint: Buffer.alloc(32, 5).toString("base64"),
};
const MAC = "0f8fad5b-d9cb-469f-a165-70867728950e";

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

beforeEach(() => link.request.mockReset());

describe("pinnedRequest", () => {
	it("sends the pin, a 10 s timeout, Accept and the JSON body through the module", async () => {
		answer(204);
		await pinnedRequest(endpoint, "POST", "/x", {
			headers: { "X-Test": "1" },
			body: { a: 1 },
		});
		expect(lastRequest()).toEqual({
			url: "https://[fe80::1]:4433/x",
			method: "POST",
			headers: { Accept: "application/json", "X-Test": "1" },
			body: '{"a":1}',
			fingerprint: endpoint.fingerprint,
			timeoutMs: 10_000,
		});
		// A bodiless call omits the body key entirely.
		answer(200);
		await pinnedRequest(endpoint, "GET", "/x");
		expect("body" in lastRequest()).toBe(false);
	});

	it("wraps any module rejection as unreachable with the native message", async () => {
		link.request.mockRejectedValueOnce(new Error("ERR_STENO_REQUEST: pin"));
		await expect(pinnedRequest(endpoint, "GET", "/x")).rejects.toMatchObject({
			name: "HandoverError",
			kind: "unreachable",
			status: null,
			message: "ERR_STENO_REQUEST: pin",
		});
		link.request.mockRejectedValueOnce("string reason");
		await expect(pinnedRequest(endpoint, "GET", "/x")).rejects.toMatchObject({
			message: "string reason",
		});
	});
});

describe("failureFor", () => {
	it.each([
		[200, null],
		[204, null],
		[304, null],
		[401, "unauthorized"],
		[403, "forbidden"],
		[404, "not-found"],
		[409, "server"],
		[413, "server"],
		[500, "server"],
	])("maps %s to %s", (status, kind) => {
		const failure = failureFor({ status, headers: {}, body: "" });
		expect(failure?.kind ?? null).toBe(kind);
		if (failure) expect(failure).toMatchObject({ status, message: /\d{3}/ });
	});
});

describe("hello", () => {
	it("GETs /v1/hello without credentials and returns the Mac's id", async () => {
		answer(200, { macID: MAC, protocol: 1 });
		expect(await hello(endpoint)).toEqual({ macID: MAC, protocol: 1 });
		expect(lastRequest()).toMatchObject({
			url: "https://[fe80::1]:4433/v1/hello",
			method: "GET",
			headers: { Accept: "application/json" },
		});
		expect(lastRequest().headers.Authorization).toBeUndefined();
	});

	it("rejects another protocol version and a malformed body as protocol errors", async () => {
		answer(200, { macID: MAC, protocol: 2 });
		await expect(hello(endpoint)).rejects.toMatchObject({
			kind: "protocol",
			message: /protocol 2, phone speaks 1/,
		});
		answer(200, "<html>");
		await expect(hello(endpoint)).rejects.toMatchObject({
			kind: "protocol",
			message: "invalid JSON",
		});
	});

	it("surfaces an HTTP failure before decoding", async () => {
		answer(503, "busy");
		await expect(hello(endpoint)).rejects.toMatchObject({
			kind: "server",
			status: 503,
		});
	});
});

describe("pair", () => {
	const request = { deviceID: "dev-1", deviceName: "iPhone" };

	it("POSTs the PairRequest with the Pairing scheme and decodes the PairResponse", async () => {
		answer(200, { token: "tok", macID: MAC, macName: "Studio" });
		expect(await pair(endpoint, "s3cret", request)).toEqual({
			token: "tok",
			macID: MAC,
			macName: "Studio",
		});
		expect(lastRequest()).toMatchObject({
			url: "https://[fe80::1]:4433/v1/pair",
			method: "POST",
			headers: { Authorization: "Pairing s3cret" },
			body: JSON.stringify(request),
		});
		expect(Object.keys(JSON.parse(lastRequest().body ?? "")).sort()).toEqual([
			"deviceID",
			"deviceName",
		]);
	});

	it("maps 403 (bad or spent secret) to forbidden and a partial body to protocol", async () => {
		answer(403);
		await expect(pair(endpoint, "used", request)).rejects.toMatchObject({
			kind: "forbidden",
			status: 403,
		});
		answer(200, { token: "tok", macID: MAC });
		await expect(pair(endpoint, "s", request)).rejects.toMatchObject({
			kind: "protocol",
			message: "macName must be a string",
		});
	});
});

describe("unpair", () => {
	it("DELETEs /v1/pairing with the bearer and tolerates 401", async () => {
		answer(204);
		await unpair(endpoint, "tok");
		expect(lastRequest()).toMatchObject({
			url: "https://[fe80::1]:4433/v1/pairing",
			method: "DELETE",
			headers: { Authorization: "Bearer tok" },
		});
		answer(401);
		await expect(unpair(endpoint, "tok")).resolves.toBeUndefined();
	});

	it("still reports other failures", async () => {
		answer(500);
		await expect(unpair(endpoint, "tok")).rejects.toBeInstanceOf(HandoverError);
		link.request.mockRejectedValueOnce(new Error("offline"));
		await expect(unpair(endpoint, "tok")).rejects.toMatchObject({
			kind: "unreachable",
		});
	});
});
