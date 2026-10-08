import { describe, expect, it, vi } from "vitest";

vi.mock("expo-file-system", () => ({}));
vi.mock("@/features/queue/queue-files", () => ({ expoQueueFiles: {} }));

import { createAdoptedOrigins, parseAdoptedOrigin } from "./adopted-origin";

const PATH = "file:///docs/sync/adopted-origin.json";
const ENDPOINT = { origin: "https://10.0.0.1:1", fingerprint: "FP-mac-a" };

/**
 * In-memory files; `failRename` makes every rename throw after removing the
 * target, as expo's move does, and `failRead` makes every read throw.
 */
function memoryFiles(initial: Record<string, string> = {}) {
	const store = new Map(Object.entries(initial));
	const calls: string[] = [];
	const state = { failRename: false, failRead: false };
	const api = {
		async readText(path: string) {
			calls.push(`read ${path}`);
			if (state.failRead) throw new Error("EACCES read");
			return store.get(path) ?? null;
		},
		async writeText(path: string, text: string) {
			calls.push(`write ${path}`);
			store.set(path, text);
		},
		async rename(from: string, to: string) {
			calls.push(`rename ${from} -> ${to}`);
			store.delete(to);
			if (state.failRename) throw new Error("EIO rename");
			const text = store.get(from);
			if (text === undefined) throw new Error(`missing ${from}`);
			store.delete(from);
			store.set(to, text);
		},
	};
	return { api, store, calls, state };
}

describe("adopted origin", () => {
	it("reads back what it wrote, through a temp file", async () => {
		const files = memoryFiles();
		const origins = createAdoptedOrigins(files.api, () => PATH);
		await origins.write(ENDPOINT);
		expect(await origins.read()).toEqual(ENDPOINT);
		expect(files.calls).toEqual([
			`write ${PATH}.tmp`,
			`rename ${PATH}.tmp -> ${PATH}`,
			`read ${PATH}`,
		]);
	});

	it("keeps the last of two writes made at once", async () => {
		const files = memoryFiles();
		const origins = createAdoptedOrigins(files.api, () => PATH);
		const second = { ...ENDPOINT, origin: "https://10.0.0.9:1" };
		await Promise.all([origins.write(ENDPOINT), origins.write(second)]);
		expect(await origins.read()).toEqual(second);
	});

	it("reads no earlier address when the file is missing or unreadable", async () => {
		const files = memoryFiles();
		const origins = createAdoptedOrigins(files.api, () => PATH);
		expect(await origins.read()).toBeNull();
		files.store.set(PATH, JSON.stringify(ENDPOINT));
		files.state.failRead = true;
		expect(await origins.read()).toBeNull();
	});

	it("reads no earlier address after a failed rename, and writes again", async () => {
		const files = memoryFiles({ [PATH]: JSON.stringify(ENDPOINT) });
		const origins = createAdoptedOrigins(files.api, () => PATH);
		files.state.failRename = true;
		const second = { ...ENDPOINT, origin: "https://10.0.0.9:1" };
		await expect(origins.write(second)).rejects.toThrow("EIO rename");
		expect(await origins.read()).toBeNull();
		files.state.failRename = false;
		await origins.write(second);
		expect(await origins.read()).toEqual(second);
	});

	it("parses only an https origin with a fingerprint", () => {
		expect(parseAdoptedOrigin(JSON.stringify(ENDPOINT))).toEqual(ENDPOINT);
		for (const text of [
			"",
			"{",
			"null",
			"[]",
			'"https://10.0.0.1:1"',
			JSON.stringify({ origin: "https://10.0.0.1:1" }),
			JSON.stringify({ ...ENDPOINT, fingerprint: "" }),
			JSON.stringify({ ...ENDPOINT, origin: "http://10.0.0.1:1" }),
			JSON.stringify({ ...ENDPOINT, origin: 1 }),
		]) {
			expect(parseAdoptedOrigin(text)).toBeNull();
		}
	});
});
