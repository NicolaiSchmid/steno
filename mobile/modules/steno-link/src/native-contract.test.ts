import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import type {
	BrowserState,
	MacService,
	PinnedRequest,
	PinnedResponse,
	ResolvedMac,
	StenoLinkEvents,
	UploadFailed,
	UploadFinished,
	UploadProgress,
	UploadSpec,
} from "./StenoLink.types";
import { CHUNK_HASH_HEADER, SERVICE_TYPE } from "./wire";

/**
 * The JS-native contract, checked by name without a Swift toolchain: the
 * Swift sources under `ios/` are parsed as text. Every JS-facing shape is a
 * `Record` struct in `Records.swift` named as its TypeScript type, so the
 * check is one field list per type, plus which record each event and
 * function hands over. Each `Record<keyof T, null>` below fails to compile
 * when the TS type changes, so both sides move together or this test breaks.
 */
const source = (relative: string) =>
	readFileSync(new URL(relative, import.meta.url), "utf8");
const swift = (file: string) => source(`../ios/${file}`);

const module = swift("StenoLinkModule.swift");
const records = swift("Records.swift");
const uploadSession = swift("UploadSession.swift");
const browser = swift("Browser.swift");
const pinnedClient = swift("PinnedSessionDelegate.swift");

function quotedStrings(text: string): string[] {
	return [...text.matchAll(/"([^"]+)"/g)].map((m) => m[1] as string);
}

function recordFields(name: string): string[] {
	const body = records.match(
		new RegExp(`struct ${name}: Record \\{([\\s\\S]*?)\\n\\}`),
	)?.[1];
	if (!body) throw new Error(`no record ${name}`);
	return [...body.matchAll(/@Field var (\w+)/g)]
		.map((m) => m[1] as string)
		.sort();
}

/** Text of one module function, from its declaration to the next one. */
function functionBody(name: string): string {
	const start = module.indexOf(`Function("${name}")`);
	if (start === -1) throw new Error(`no function ${name}`);
	const rest = module.slice(start + 1);
	const next = rest.search(/(Async)?Function\("/);
	return next === -1 ? rest : rest.slice(0, next);
}

/** The expression handed over with every `emit("name", …)` / `eventSink?("name", …)`. */
function eventPayloads(text: string, event: string): string[] {
	return [
		...text.matchAll(
			new RegExp(`"${event}",\\s*([\\s\\S]*?)\\.toDictionary\\(\\)`, "g"),
		),
	].map((m) => (m[1] as string).trim());
}

const keys = <T extends object>(record: Record<keyof T, null>) =>
	Object.keys(record).sort();

describe("Records.swift names every bridge type with the TypeScript fields", () => {
	it("MacService, ResolvedMac, BrowserState", () => {
		expect(recordFields("MacService")).toEqual(
			keys<MacService>({ name: null, macID: null }),
		);
		expect(recordFields("ResolvedMac")).toEqual(
			keys<ResolvedMac>({ host: null, port: null }),
		);
		expect(recordFields("BrowserState")).toEqual(
			keys<BrowserState>({ state: null, policyDenied: null }),
		);
	});

	it("PinnedRequest and PinnedResponse", () => {
		expect(recordFields("PinnedRequest")).toEqual(
			keys<PinnedRequest>({
				url: null,
				method: null,
				headers: null,
				body: null,
				fingerprint: null,
				timeoutMs: null,
			}),
		);
		expect(recordFields("PinnedResponse")).toEqual(
			keys<PinnedResponse>({ status: null, headers: null, body: null }),
		);
	});

	it("UploadSpec, UploadProgress, UploadFinished, UploadFailed", () => {
		expect(recordFields("UploadSpec")).toEqual(
			keys<UploadSpec>({
				taskID: null,
				url: null,
				headers: null,
				fingerprint: null,
				filePath: null,
				offset: null,
				length: null,
			}),
		);
		expect(recordFields("UploadProgress")).toEqual(
			keys<UploadProgress>({ taskID: null, bytesSent: null, totalBytes: null }),
		);
		expect(recordFields("UploadFinished")).toEqual(
			keys<UploadFinished>({ taskID: null, status: null, body: null }),
		);
		expect(recordFields("UploadFailed")).toEqual(
			keys<UploadFailed>({ taskID: null, message: null, retryable: null }),
		);
	});

	it("declares no other record, so nothing crosses the bridge untyped", () => {
		const declared = [...records.matchAll(/struct (\w+): Record/g)]
			.map((m) => m[1] as string)
			.sort();
		expect(declared).toEqual(
			[
				"MacService",
				"ResolvedMac",
				"BrowserState",
				"PinnedRequest",
				"PinnedResponse",
				"UploadSpec",
				"UploadProgress",
				"UploadFinished",
				"UploadFailed",
			].sort(),
		);
		expect(module).not.toMatch(/: Record \{/);
		expect(module).not.toMatch(/promise\.resolve\(\[/);
	});
});

describe("StenoLinkModule definition", () => {
	it("declares exactly the events StenoLinkEvents names", () => {
		const declared = module.match(/Events\(([\s\S]*?)\)/)?.[1] ?? "";
		expect(quotedStrings(declared).sort()).toEqual(
			keys<StenoLinkEvents>({
				serviceFound: null,
				serviceLost: null,
				browserState: null,
				uploadProgress: null,
				uploadFinished: null,
				uploadFailed: null,
			}),
		);
	});

	it("exposes exactly the functions the TypeScript module declares", () => {
		const declaration = source("./native-module.ts").match(
			/declare class StenoLinkNativeModule[^{]*\{([\s\S]*?)\n\}/,
		)?.[1];
		if (!declaration) throw new Error("no StenoLinkNativeModule declaration");
		const declared = [...declaration.matchAll(/^\t(\w+)\(/gm)]
			.map((m) => m[1] as string)
			.sort();
		expect(declared).toHaveLength(8);
		const defined = [...module.matchAll(/(?:Async)?Function\("(\w+)"\)/g)]
			.map((m) => m[1] as string)
			.sort();
		expect(defined).toEqual(declared);
	});

	it("takes PinnedRequest and UploadSpec as arguments and resolves the records", () => {
		expect(functionBody("request")).toContain("(request: PinnedRequest,");
		expect(functionBody("request")).toContain(
			"promise.resolve(response.toDictionary())",
		);
		expect(pinnedClient).toContain(
			"perform(_ request: PinnedRequest, completion: @escaping (Result<PinnedResponse, Error>) -> Void)",
		);
		expect(functionBody("startUpload")).toContain("(spec: UploadSpec)");
		expect(uploadSession).toContain("func start(_ spec: UploadSpec) throws");
		expect(functionBody("resolve")).toContain(
			"promise.resolve(resolved.toDictionary())",
		);
		expect(browser).toContain(
			"completion: @escaping (Result<ResolvedMac, Error>) -> Void",
		);
		expect(browser).toContain("-> ResolvedMac?");
	});
});

describe("event payloads are the matching records", () => {
	it("upload events hand over UploadProgress, UploadFinished and UploadFailed", () => {
		const progress = eventPayloads(uploadSession, "uploadProgress");
		expect(progress).toHaveLength(1);
		expect(progress[0]).toMatch(/^UploadProgress\(/);

		const finished = eventPayloads(uploadSession, "uploadFinished");
		expect(finished).toHaveLength(1);
		expect(finished[0]).toMatch(/^UploadFinished\(/);

		const failed = eventPayloads(uploadSession, "uploadFailed");
		expect(failed.length).toBeGreaterThanOrEqual(2);
		for (const payload of failed) expect(payload).toMatch(/^UploadFailed\(/);
	});

	it("browser events hand over BrowserState with every state value, and MacService", () => {
		const states = eventPayloads(browser, "browserState");
		expect(states).toHaveLength(4);
		for (const payload of states) expect(payload).toMatch(/^BrowserState\(/);
		const values: Record<BrowserState["state"], null> = {
			ready: null,
			waiting: null,
			failed: null,
			cancelled: null,
		};
		expect(states.map((b) => b.match(/state: "(\w+)"/)?.[1]).sort()).toEqual(
			Object.keys(values).sort(),
		);

		for (const event of ["serviceFound", "serviceLost"]) {
			const payloads = eventPayloads(browser, event);
			expect(payloads.length).toBeGreaterThanOrEqual(1);
			for (const payload of payloads) {
				expect(payload).toMatch(/^Browser\.service\(/);
			}
		}
		expect(browser).toContain(
			"static func service(_ result: NWBrowser.Result) -> MacService",
		);
	});
});

describe("wire constants shared with Swift", () => {
	it("browses the same service type and sets the same chunk hash header", () => {
		expect(browser).toContain(`serviceType = "${SERVICE_TYPE}"`);
		expect(uploadSession).toContain(`chunkHashHeader = "${CHUNK_HASH_HEADER}"`);
	});

	it("reads the Bonjour mac id from the TXT key the Mac publishes", () => {
		expect(browser).toContain('txt.dictionary["id"]');
	});
});
