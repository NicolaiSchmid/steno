// @vitest-environment happy-dom

import type {
	PinnedRequest,
	UploadFailed,
	UploadFinished,
	UploadSpec,
} from "@modules/steno-link";
import type { MacEndpoint } from "@modules/steno-link/native";
import { act, StrictMode, useState } from "react";
import { flushSync } from "react-dom";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { Pairing } from "@/features/pairing/pairing-store";
import {
	addRecording,
	EMPTY_INDEX,
	findRecording,
	type QueueIndex,
	setState,
} from "@/features/queue/queue-index";
import { BACKOFF_BASE_MS, taskIDs } from "./upload-coordinator";
import {
	RERESOLVE_INTERVAL_MS,
	useUploadCoordinator,
} from "./use-upload-coordinator";

/**
 * The hook over a fake native module and a fake discovery that sees both
 * Macs: Mac B's `resolve` answers only when the test releases it, and every
 * pinned request and background upload is recorded with its URL and
 * bearer. The real recording client and executor run on top. The pairing,
 * a re-pairing still saving (`replacing`), the queue, each Mac's address,
 * the addresses that answer nothing or present another certificate, the
 * address the last process chose and the app state are plain state the test
 * drives. It renders under `StrictMode`, so every effect runs twice.
 */
const fake = vi.hoisted(() => {
	const defaults = () => ({
		listeners: new Map<string, Set<(event: unknown) => void>>(),
		appStateListeners: new Set<(state: string) => void>(),
		sent: [] as { url: string; auth: string | undefined }[],
		/** The requests and uploads that got past the pin, in order. */
		reached: [] as { url: string; auth: string | undefined }[],
		/** The pin of every request and upload, in order. */
		pins: [] as string[],
		/** Every service name resolved, in order. */
		resolves: [] as string[],
		/** The address each Mac's service resolves to. */
		hosts: { "Mac A": "10.0.0.1", "Mac B": "10.0.0.2" } as Record<
			string,
			string
		>,
		/** Addresses where nothing answers: a request there fails to connect. */
		down: new Set<string>(),
		/**
		 * The certificate an address presents, where it is not the pairing's
		 * Mac's: a request there with another pin fails as a rejected pin.
		 */
		certificates: {} as Record<string, string>,
		/** The address the last process chose (`adopted-origin.ts`), and its writes. */
		adoptedOrigin: null as MacEndpoint | null,
		writes: [] as MacEndpoint[],
		/** Every queue file deleted, with the last request sent before it. */
		deleted: [] as { fileName: string; after: string | undefined }[],
		failResolve: false,
		/** When set, the Mac answers a `PUT` with this status. */
		putStatus: null as number | null,
		/** The task ids `pendingUploads` reports, and those cancelled. */
		pending: [] as string[],
		cancelled: [] as string[],
		releaseMacB: null as (() => void) | null,
		/** When set, the next such call waits for it. */
		holdWrite: null as Promise<void> | null,
		holdUpload: null as Promise<void> | null,
		holdRequest: null as Promise<void> | null,
		holdDeviceName: null as Promise<void> | null,
		/** The chunks the Mac answers it has. */
		receivedChunks: [] as number[],
		replacing: null as string | null,
	});
	const fake = {
		...defaults(),
		reset: () => Object.assign(fake, defaults()),
		pairing: null as unknown,
		index: null as unknown,
		update: null as unknown,
		coordinator: null as unknown,
		currentToken: () =>
			fake.replacing ??
			(fake.pairing as { token: string } | null)?.token ??
			null,
		clearIfCurrent: async () => false,
		emit(event: string, payload: unknown) {
			for (const listener of fake.listeners.get(event) ?? []) listener(payload);
		},
	};
	return fake;
});

/** Mac A's endpoint at `host`, as `adopted-origin.ts` keeps it. */
function macAAt(host: string): MacEndpoint {
	return { origin: `https://${host}:1`, fingerprint: "FP-mac-a" };
}

function hostOf(url: string) {
	return new URL(url).hostname;
}

function pinRejected(url: string, fingerprint: string) {
	const certificate = fake.certificates[hostOf(url)];
	return certificate !== undefined && certificate !== fingerprint;
}

const link = {
	addListener(event: string, listener: (event: unknown) => void) {
		const set = fake.listeners.get(event) ?? new Set();
		set.add(listener);
		fake.listeners.set(event, set);
		return { remove: () => set.delete(listener) };
	},
	resolve(serviceName: string) {
		fake.resolves.push(serviceName);
		if (fake.failResolve) {
			return Promise.reject(new Error(`Resolving ${serviceName} timed out`));
		}
		const mac = { host: fake.hosts[serviceName], port: 1 };
		if (serviceName !== "Mac B") return Promise.resolve(mac);
		return new Promise((resolve) => {
			fake.releaseMacB = () => resolve(mac);
		});
	},
	async pendingUploads() {
		return [...fake.pending];
	},
	async startUpload(spec: UploadSpec) {
		fake.sent.push({ url: spec.url, auth: spec.headers.Authorization });
		fake.pins.push(spec.fingerprint);
		if (!pinRejected(spec.url, spec.fingerprint)) {
			fake.reached.push({ url: spec.url, auth: spec.headers.Authorization });
		}
		await fake.holdUpload;
	},
	async cancelUpload(taskID: string) {
		fake.cancelled.push(taskID);
	},
	async request(request: PinnedRequest) {
		fake.sent.push({ url: request.url, auth: request.headers.Authorization });
		fake.pins.push(request.fingerprint);
		await fake.holdRequest;
		if (fake.down.has(hostOf(request.url))) {
			throw new Error("Could not connect to the server.");
		}
		if (pinRejected(request.url, request.fingerprint)) {
			throw new Error("The certificate for this server is invalid.");
		}
		fake.reached.push({
			url: request.url,
			auth: request.headers.Authorization,
		});
		if (request.method === "PUT" && fake.putStatus !== null) {
			return { status: fake.putStatus, headers: {}, body: "" };
		}
		if (request.method === "POST") {
			return { status: 200, headers: {}, body: '{"meetingID":"m"}' };
		}
		return {
			status: 200,
			headers: {},
			body: JSON.stringify({
				state: "receiving",
				receivedChunks: fake.receivedChunks,
			}),
		};
	},
};

vi.mock("expo", () => ({ requireNativeModule: () => link }));
vi.mock("react-native", () => ({
	AppState: {
		currentState: "active",
		addEventListener(_event: "change", listener: (state: string) => void) {
			fake.appStateListeners.add(listener);
			return { remove: () => fake.appStateListeners.delete(listener) };
		},
	},
}));
vi.mock("@/features/discovery/use-mac-discovery", () => ({
	useMacDiscovery: () => ({
		services: [
			{ name: "Mac A", macID: "mac-a" },
			{ name: "Mac B", macID: "mac-b" },
		],
		browser: null,
	}),
	restartBrowsing: () => {},
}));
vi.mock("@/features/pairing/PairingProvider", () => ({
	usePairing: () => ({
		pairing: fake.pairing,
		ready: true,
		currentToken: fake.currentToken,
		clearIfCurrent: fake.clearIfCurrent,
	}),
}));
vi.mock("@/features/pairing/pairing-store", () => ({
	deviceIdentity: async () => {
		await fake.holdDeviceName;
		return { deviceName: "Phone" };
	},
}));
vi.mock("@/features/queue/QueueProvider", () => ({
	useQueue: () => ({ index: fake.index, ready: true, update: fake.update }),
}));
vi.mock("@/features/queue/queue-files", () => ({
	queuedFile: (fileName: string) => ({
		exists: true,
		uri: `file:///queue/${fileName}`,
		delete() {
			fake.deleted.push({ fileName, after: fake.sent.at(-1)?.url });
		},
	}),
}));
vi.mock("./adopted-origin", () => ({
	adoptedOrigin: {
		read: async () => fake.adoptedOrigin,
		write: async (endpoint: MacEndpoint) => {
			fake.writes.push(endpoint);
			await fake.holdWrite;
			fake.adoptedOrigin = endpoint;
		},
	},
}));

(
	globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

function pairingWith(mac: string, token: string): Pairing {
	return {
		mac: {
			macID: mac,
			macName: mac,
			fingerprint: `FP-${mac}`,
			pairedAt: "2026-09-25T10:00:00.000Z",
		},
		token,
	};
}

const A = pairingWith("mac-a", "token-a");
const B = pairingWith("mac-b", "token-b");

let root: Root | null = null;

async function mount(initial: QueueIndex, pairing: Pairing) {
	let setPairing!: (pairing: Pairing | null) => void;
	let latest = initial;
	let setIndex!: (index: QueueIndex) => void;
	fake.update = async (transform: (index: QueueIndex) => QueueIndex) => {
		latest = transform(latest);
		// Commit at once: under `act` the render would wait for the tick
		// loop, which re-plans right after each update, to run out of work,
		// so it would re-plan on the old index forever.
		flushSync(() => setIndex(latest));
		return latest;
	};
	function Probe() {
		const [current, setCurrent] = useState<Pairing | null>(pairing);
		const [index, setIndexState] = useState(initial);
		setPairing = setCurrent;
		setIndex = setIndexState;
		fake.pairing = current;
		fake.index = index;
		fake.coordinator = useUploadCoordinator();
		return null;
	}
	root = createRoot(document.createElement("div"));
	await act(async () =>
		root?.render(
			<StrictMode>
				<Probe />
			</StrictMode>,
		),
	);
	await settle();
	return {
		row: (id: string) => findRecording(latest, id),
		/** Queues a one-chunk recording, as the recorder does after a stop. */
		add: async (id: string) => {
			await act(async () => {
				latest = withRecording(latest, id, 100);
				flushSync(() => setIndex(latest));
			});
			await settle();
		},
		reachable: () =>
			(fake.coordinator as ReturnType<typeof useUploadCoordinator>).reachable,
		repair: (next: Pairing | null) => act(async () => setPairing(next)),
	};
}

/** Lets the tick loop and the effects run until nothing is left to do. */
async function settle() {
	for (let i = 0; i < 5; i++) {
		await act(async () => {
			await new Promise((resolve) => setTimeout(resolve, 0));
		});
	}
}

/** One-chunk recordings, oldest first. */
function queued(...ids: string[]) {
	return ids.reduce((index, id) => withRecording(index, id, 100), EMPTY_INDEX);
}

function withRecording(index: QueueIndex, id: string, byteCount: number) {
	return addRecording(index, {
		recordingID: id,
		fileName: `${id}.m4a`,
		startedAt: "2026-09-25T09:00:00.000Z",
		durationSeconds: 60,
		byteCount,
		sha256: Buffer.alloc(32, 9).toString("base64"),
		chunkSize: 1024,
	});
}

function finished(id: string): UploadFinished {
	return { taskID: taskIDs.chunk(id, 0), status: 204, body: "" };
}

function connectFailed(id: string): UploadFailed {
	return {
		taskID: taskIDs.chunk(id, 0),
		message: "Could not connect to the server.",
		retryable: true,
	};
}

async function elapse(ms: number) {
	await act(() => vi.advanceTimersByTimeAsync(ms));
	await settle();
}

function moveApp(state: "active" | "background") {
	act(() => {
		for (const listener of fake.appStateListeners) listener(state);
	});
}

function cancelled(id: string): UploadFailed {
	return {
		taskID: taskIDs.chunk(id, 0),
		message: "cancelled",
		retryable: false,
	};
}

beforeEach(() => {
	fake.reset();
	vi.spyOn(console, "warn").mockImplementation(() => {});
	// Fake timers and clock, so a backoff and the resolve timer can elapse.
	vi.useFakeTimers({
		shouldAdvanceTime: true,
		toFake: [
			"setTimeout",
			"clearTimeout",
			"setInterval",
			"clearInterval",
			"Date",
		],
	});
});
afterEach(() => {
	act(() => root?.unmount());
	root = null;
	vi.useRealTimers();
	vi.restoreAllMocks();
});

describe("useUploadCoordinator", () => {
	it("sends nothing with the old pairing's token or Mac after a re-pairing", async () => {
		const h = await mount(queued("a", "bb"), A);
		expect(fake.sent).toContainEqual({
			url: "https://10.0.0.1:1/v1/recordings/a",
			auth: "Bearer token-a",
		});
		expect(h.row("a")?.state).toBe("uploading");

		await h.repair(B);
		expect(h.reachable()).toBe(false);
		const sentUnderA = fake.sent.length;
		// The chunk sent under A lands, which would plan `complete` next.
		await act(async () =>
			fake.emit("uploadFinished", {
				taskID: taskIDs.chunk("a", 0),
				status: 204,
				body: "",
			} satisfies UploadFinished),
		);
		await settle();
		expect(fake.sent.slice(sentUnderA)).toEqual([]);

		await act(async () => fake.releaseMacB?.());
		await settle();
		const sentSince = fake.sent.slice(sentUnderA);
		expect(sentSince).toContainEqual({
			url: "https://10.0.0.2:1/v1/recordings/a/complete",
			auth: "Bearer token-b",
		});
		for (const request of sentSince) {
			expect(request.url.startsWith("https://10.0.0.2:1/")).toBe(true);
			expect(request.auth).toBe("Bearer token-b");
		}
	});

	it("sends with the new token to the same Mac after re-pairing it", async () => {
		const h = await mount(queued("a"), A);
		await h.repair({ ...A, token: "token-a2" });
		const sentUnderA = fake.sent.length;
		await act(async () => fake.emit("uploadFinished", finished("a")));
		await settle();
		const sentSince = fake.sent.slice(sentUnderA);
		expect(sentSince).toContainEqual({
			url: "https://10.0.0.1:1/v1/recordings/a/complete",
			auth: "Bearer token-a2",
		});
		for (const request of sentSince) {
			expect(request.auth).toBe("Bearer token-a2");
		}
		expect(fake.cancelled).toEqual([]);
	});

	it("sends nothing after the phone unpairs", async () => {
		const h = await mount(queued("a"), A);
		await h.repair(null);
		const sentUnderA = fake.sent.length;
		await act(async () => fake.emit("uploadFinished", finished("a")));
		await settle();
		expect(fake.sent.slice(sentUnderA)).toEqual([]);
		expect(h.reachable()).toBe(false);
	});

	it("catches a background upload event whose queue update fails", async () => {
		const warn = vi.mocked(console.warn);
		await mount(queued("a"), A);
		fake.update = async () => {
			throw new Error("The recordings list could not be read");
		};
		await act(async () => fake.emit("uploadFinished", finished("a")));
		await act(async () => fake.emit("uploadFailed", cancelled("a")));
		await settle();
		expect(
			warn.mock.calls.filter(
				([message]) => message === "[sync] upload event failed",
			),
		).toHaveLength(2);
	});

	describe("while a re-pairing is still saving", () => {
		it("plans nothing under the old pairing, then uploads to the new Mac", async () => {
			const h = await mount(queued("a", "bb"), A);
			expect(h.row("bb")?.state).toBe("queued");
			fake.replacing = "token-b";
			const sentUnderA = fake.sent.length;
			// The re-pairing cancels the chunk in flight, which frees the slot
			// for "bb" while the old pairing is still the committed one.
			await act(async () => fake.emit("uploadFailed", cancelled("a")));
			await settle();
			expect(fake.sent.slice(sentUnderA)).toEqual([]);
			expect(h.row("bb")).toMatchObject({ state: "queued", attempts: 0 });

			fake.replacing = null;
			await h.repair(B);
			await act(async () => fake.releaseMacB?.());
			await settle();
			expect(fake.sent.slice(sentUnderA)).toContainEqual({
				url: "https://10.0.0.2:1/v1/recordings/bb",
				auth: "Bearer token-b",
			});
			for (const request of fake.sent.slice(sentUnderA)) {
				expect(request.auth).toBe("Bearer token-b");
			}
		});

		it("refuses the next request of a tick already running", async () => {
			const deviceName = Promise.withResolvers<void>();
			fake.holdDeviceName = deviceName.promise;
			const h = await mount(queued("a"), A);
			expect(fake.sent).toEqual([]);
			fake.replacing = "token-b";
			await act(async () => deviceName.resolve());
			await settle();
			expect(fake.sent).toEqual([]);
			expect(h.row("a")).toMatchObject({
				state: "queued",
				lastError: "Retrying",
			});
		});

		it("cancels a chunk whose task was still being created", async () => {
			const upload = Promise.withResolvers<void>();
			fake.holdUpload = upload.promise;
			// The Mac has chunk 0 of two, so the chunk in creation is chunk 1.
			fake.receivedChunks = [0];
			await mount(withRecording(EMPTY_INDEX, "a", 2000), A);
			expect(fake.sent.at(-1)?.url).toBe(
				"https://10.0.0.1:1/v1/recordings/a/chunks/1",
			);
			fake.replacing = "token-b";
			await act(async () => upload.resolve());
			await settle();
			expect(fake.cancelled).toEqual([taskIDs.chunk("a", 1)]);
		});

		it("refuses the status refresh under the old pairing", async () => {
			// A request that goes out anyway is held, so the tick ends there.
			fake.holdRequest = new Promise(() => {});
			fake.replacing = "token-b";
			await mount(setState(queued("a"), "a", "uploading"), A);
			expect(fake.sent).toEqual([]);
		});

		it("goes on under the old pairing when the save fails", async () => {
			const h = await mount(queued("a", "bb"), A);
			fake.replacing = "token-b";
			await act(async () => fake.emit("uploadFailed", cancelled("a")));
			await settle();
			expect(h.row("bb")?.state).toBe("queued");

			fake.replacing = null;
			await elapse(1000);
			expect(fake.sent).toContainEqual({
				url: "https://10.0.0.1:1/v1/recordings/bb",
				auth: "Bearer token-a",
			});
		});

		it("stops looking again once the screen is gone", async () => {
			// A request that goes out anyway is held, so the tick ends there.
			fake.holdRequest = new Promise(() => {});
			fake.replacing = "token-b";
			await mount(queued("a"), A);
			act(() => root?.unmount());
			root = null;

			fake.replacing = null;
			await elapse(1000);
			expect(fake.sent).toEqual([]);
		});
	});

	describe("when the Mac's address changes under the same name", () => {
		it("resolves again after a request fails to connect, and retries at the new address", async () => {
			const h = await mount(EMPTY_INDEX, A);
			expect(h.reachable()).toBe(true);
			// A new lease: the old address answers nothing, the name stays.
			fake.hosts["Mac A"] = "10.0.0.9";
			fake.down.add("10.0.0.1");
			const resolved = fake.resolves.length;
			await h.add("a");
			expect(fake.sent[0]).toEqual({
				url: "https://10.0.0.1:1/v1/recordings/a",
				auth: "Bearer token-a",
			});
			expect(h.row("a")).toMatchObject({ state: "queued", attempts: 1 });
			expect(fake.resolves.length).toBe(resolved + 1);

			await elapse(BACKOFF_BASE_MS);
			expect(fake.sent).toContainEqual({
				url: "https://10.0.0.9:1/v1/recordings/a",
				auth: "Bearer token-a",
			});
			expect(h.row("a")?.state).toBe("uploading");
			// Only the probe of whether the old address still answers went
			// there, and every request kept the pairing's pin.
			for (const request of fake.sent.slice(1)) {
				if (hostOf(request.url) === "10.0.0.1") {
					expect(request.url).toBe("https://10.0.0.1:1/v1/hello");
				}
			}
			expect(new Set(fake.pins)).toEqual(new Set(["FP-mac-a"]));
		});

		it("resolves again after a failure to connect, not after an answer or the app's own cancel", async () => {
			const h = await mount(EMPTY_INDEX, A);
			const resolved = fake.resolves.length;
			fake.putStatus = 500;
			await h.add("a");
			expect(h.row("a")).toMatchObject({
				state: "queued",
				lastError: "The Mac answered 500",
			});
			await act(async () =>
				fake.emit("uploadFinished", { ...finished("a"), status: 500 }),
			);
			await act(async () => fake.emit("uploadFailed", cancelled("a")));
			await settle();
			expect(fake.resolves.length).toBe(resolved);

			await act(async () => fake.emit("uploadFailed", connectFailed("a")));
			await settle();
			expect(fake.resolves.length).toBe(resolved + 1);
		});

		it("resolves on the timer only while uploads are queued in the foreground", async () => {
			const h = await mount(EMPTY_INDEX, A);
			const resolved = fake.resolves.length;
			await elapse(2 * RERESOLVE_INTERVAL_MS);
			expect(fake.resolves.length).toBe(resolved);

			// Its chunk is out, so the row stays `uploading`.
			await h.add("a");
			expect(h.row("a")?.state).toBe("uploading");
			await elapse(RERESOLVE_INTERVAL_MS);
			expect(fake.resolves.length).toBe(resolved + 1);

			moveApp("background");
			await elapse(2 * RERESOLVE_INTERVAL_MS);
			expect(fake.resolves.length).toBe(resolved + 1);
			moveApp("active");
			await settle();
			await elapse(RERESOLVE_INTERVAL_MS);
			expect(fake.resolves.length).toBe(resolved + 2);

			await act(async () => fake.emit("uploadFinished", finished("a")));
			await settle();
			expect(h.row("a")?.state).toBe("delivered");
			await elapse(2 * RERESOLVE_INTERVAL_MS);
			expect(fake.resolves.length).toBe(resolved + 2);
		});

		it("retries a failed resolve on the timer", async () => {
			const warn = vi.mocked(console.warn);
			fake.failResolve = true;
			const h = await mount(queued("a"), A);
			expect(h.reachable()).toBe(false);
			expect(fake.sent).toEqual([]);
			expect(warn).toHaveBeenCalledWith(
				"[sync] resolve failed",
				expect.any(Error),
			);

			fake.failResolve = false;
			await elapse(RERESOLVE_INTERVAL_MS);
			expect(h.reachable()).toBe(true);
			expect(fake.sent).toContainEqual({
				url: "https://10.0.0.1:1/v1/recordings/a",
				auth: "Bearer token-a",
			});
		});

		it("cancels the chunks out to the old address and sends them to the new one", async () => {
			const h = await mount(queued("a"), A);
			expect(fake.sent.at(-1)?.url).toBe(
				"https://10.0.0.1:1/v1/recordings/a/chunks/0",
			);
			// The background session would retry this chunk at the old
			// address; no request fails, so only the timer finds the new one.
			fake.pending = [taskIDs.chunk("a", 0)];
			fake.hosts["Mac A"] = "10.0.0.9";
			fake.down.add("10.0.0.1");
			await elapse(RERESOLVE_INTERVAL_MS);
			expect(fake.cancelled).toEqual([taskIDs.chunk("a", 0)]);

			fake.pending = [];
			await act(async () => fake.emit("uploadFailed", cancelled("a")));
			await settle();
			expect(h.row("a")).toMatchObject({ state: "queued", attempts: 1 });
			await elapse(BACKOFF_BASE_MS);
			expect(fake.sent.at(-1)).toEqual({
				url: "https://10.0.0.9:1/v1/recordings/a/chunks/0",
				auth: "Bearer token-a",
			});
			expect(h.row("a")?.state).toBe("uploading");
			expect(fake.adoptedOrigin).toEqual(macAAt("10.0.0.9"));

			// The file stays on the phone until `complete` answers.
			expect(fake.deleted).toEqual([]);
			await act(async () => fake.emit("uploadFinished", finished("a")));
			await settle();
			expect(h.row("a")?.state).toBe("delivered");
			expect(fake.deleted).toEqual([
				{
					fileName: "a.m4a",
					after: "https://10.0.0.9:1/v1/recordings/a/complete",
				},
			]);
		});

		it("switches when the old address is held by another certificate, and sends it no bearer", async () => {
			const h = await mount(queued("a"), A);
			fake.pending = [taskIDs.chunk("a", 0)];
			// Another device took the old address: it answers, but not with
			// the pairing's certificate.
			fake.hosts["Mac A"] = "10.0.0.9";
			fake.certificates["10.0.0.1"] = "FP-other";
			const sentBefore = fake.sent.length;
			const reachedBefore = fake.reached.length;
			await elapse(RERESOLVE_INTERVAL_MS);
			expect(fake.cancelled).toEqual([taskIDs.chunk("a", 0)]);

			fake.pending = [];
			await act(async () => fake.emit("uploadFailed", cancelled("a")));
			await elapse(BACKOFF_BASE_MS);
			expect(fake.sent.at(-1)).toEqual({
				url: "https://10.0.0.9:1/v1/recordings/a/chunks/0",
				auth: "Bearer token-a",
			});
			expect(h.row("a")?.state).toBe("uploading");
			const toOld = (requests: typeof fake.sent) =>
				requests.filter((r) => hostOf(r.url) === "10.0.0.1");
			expect(toOld(fake.reached.slice(reachedBefore))).toEqual([]);
			expect(toOld(fake.sent.slice(sentBefore))).toEqual([
				{ url: "https://10.0.0.1:1/v1/hello", auth: undefined },
			]);
		});

		it("cancels a chunk to the old address whose task was still being created", async () => {
			const upload = Promise.withResolvers<void>();
			fake.holdUpload = upload.promise;
			await mount(queued("a"), A);
			expect(fake.sent.at(-1)?.url).toBe(
				"https://10.0.0.1:1/v1/recordings/a/chunks/0",
			);
			fake.hosts["Mac A"] = "10.0.0.9";
			fake.down.add("10.0.0.1");
			await elapse(RERESOLVE_INTERVAL_MS);
			expect(fake.cancelled).toEqual([]);
			await act(async () => upload.resolve());
			await settle();
			expect(fake.cancelled).toEqual([taskIDs.chunk("a", 0)]);
		});

		it("keeps the address in use while it still answers, and writes nothing", async () => {
			const h = await mount(queued("a"), A);
			fake.pending = [taskIDs.chunk("a", 0)];
			// A Mac on Wi-Fi and Ethernet of one LAN resolves to either.
			fake.hosts["Mac A"] = "10.0.0.9";
			const written = fake.writes.length;
			await elapse(RERESOLVE_INTERVAL_MS);
			expect(fake.cancelled).toEqual([]);
			expect(fake.writes.length).toBe(written);
			expect(fake.sent.at(-1)?.url).toBe("https://10.0.0.1:1/v1/hello");

			fake.pending = [];
			await act(async () => fake.emit("uploadFinished", finished("a")));
			await settle();
			expect(fake.sent.at(-1)?.url).toBe(
				"https://10.0.0.1:1/v1/recordings/a/complete",
			);
			expect(h.row("a")?.state).toBe("delivered");
			expect(fake.sent.some((r) => hostOf(r.url) === "10.0.0.9")).toBe(false);
		});

		it("starts no second resolve while one runs, and one more after it", async () => {
			await mount(EMPTY_INDEX, B);
			const resolved = fake.resolves.length;
			await act(async () => fake.emit("uploadFailed", connectFailed("a")));
			await act(async () => fake.emit("uploadFailed", connectFailed("a")));
			await settle();
			expect(fake.resolves.length).toBe(resolved);

			await act(async () => fake.releaseMacB?.());
			await settle();
			expect(fake.resolves.length).toBe(resolved + 1);
			await act(async () => fake.releaseMacB?.());
			await settle();
			expect(fake.resolves.length).toBe(resolved + 1);
		});

		it("adopts and writes nothing from a resolve a re-pairing overtook", async () => {
			const h = await mount(EMPTY_INDEX, B);
			await h.repair(A);
			await settle();
			await act(async () => fake.releaseMacB?.());
			await settle();
			await h.add("a");
			expect(fake.cancelled).toEqual([]);
			expect(h.row("a")?.state).toBe("uploading");
			expect(fake.adoptedOrigin).toEqual(macAAt("10.0.0.1"));
		});

		describe("after a relaunch, with a chunk the last process left out", () => {
			beforeEach(() => {
				fake.pending = [taskIDs.chunk("a", 0)];
				fake.hosts["Mac A"] = "10.0.0.9";
			});
			// Two chunks: chunk 0 is the one left out, so the app starts chunk 1.
			const uploadingA = () =>
				setState(withRecording(EMPTY_INDEX, "a", 2000), "a", "uploading");
			const withBearerTo = (host: string) =>
				fake.sent.filter((r) => r.auth && hostOf(r.url) === host);

			it("cancels and writes nothing while the persisted address still answers", async () => {
				fake.adoptedOrigin = macAAt("10.0.0.1");
				await mount(uploadingA(), A);
				await elapse(RERESOLVE_INTERVAL_MS);
				expect(fake.cancelled).toEqual([]);
				expect(fake.writes).toEqual([]);
				expect(fake.sent).toContainEqual({
					url: "https://10.0.0.1:1/v1/recordings/a/chunks/1",
					auth: "Bearer token-a",
				});
				expect(withBearerTo("10.0.0.9")).toEqual([]);
			});

			it("cancels when it does not", async () => {
				fake.adoptedOrigin = macAAt("10.0.0.1");
				fake.down.add("10.0.0.1");
				await mount(uploadingA(), A);
				expect(fake.cancelled).toEqual([taskIDs.chunk("a", 0)]);
				expect(fake.adoptedOrigin).toEqual(macAAt("10.0.0.9"));
				expect(fake.sent).toContainEqual({
					url: "https://10.0.0.9:1/v1/recordings/a/chunks/1",
					auth: "Bearer token-a",
				});
				expect(withBearerTo("10.0.0.1")).toEqual([]);
			});

			it("writes the new address before it cancels or sends anything there", async () => {
				const write = Promise.withResolvers<void>();
				fake.holdWrite = write.promise;
				fake.adoptedOrigin = macAAt("10.0.0.1");
				fake.down.add("10.0.0.1");
				await mount(uploadingA(), A);
				expect(fake.writes).toEqual([macAAt("10.0.0.9")]);
				expect(fake.cancelled).toEqual([]);
				expect(withBearerTo("10.0.0.9")).toEqual([]);

				await act(async () => write.resolve());
				await settle();
				expect(fake.cancelled).toEqual([taskIDs.chunk("a", 0)]);
				expect(fake.sent).toContainEqual({
					url: "https://10.0.0.9:1/v1/recordings/a/chunks/1",
					auth: "Bearer token-a",
				});
			});

			it("cancels nothing from a round that ends while its write is out", async () => {
				const write = Promise.withResolvers<void>();
				fake.holdWrite = write.promise;
				fake.adoptedOrigin = macAAt("10.0.0.1");
				fake.down.add("10.0.0.1");
				await mount(uploadingA(), A);
				act(() => root?.unmount());
				root = null;
				await act(async () => write.resolve());
				await settle();
				expect(fake.cancelled).toEqual([]);
				expect(withBearerTo("10.0.0.9")).toEqual([]);
			});

			it("cancels nothing when no earlier address is known for the pairing", async () => {
				// Persisted under another pairing's certificate, at an address
				// that answers nothing.
				fake.adoptedOrigin = {
					origin: "https://10.0.0.7:1",
					fingerprint: "FP-mac-b",
				};
				fake.down.add("10.0.0.7");
				await mount(uploadingA(), A);
				await elapse(RERESOLVE_INTERVAL_MS);
				expect(fake.cancelled).toEqual([]);
				expect(fake.sent).toContainEqual({
					url: "https://10.0.0.9:1/v1/recordings/a/chunks/1",
					auth: "Bearer token-a",
				});
			});
		});
	});
});
