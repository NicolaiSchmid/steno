// @vitest-environment happy-dom
import type {
	PinnedRequest,
	UploadFailed,
	UploadFinished,
	UploadSpec,
} from "@modules/steno-link";
import { act, useState } from "react";
import { flushSync } from "react-dom";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { Pairing } from "@/features/pairing/pairing-store";
import {
	addRecording,
	EMPTY_INDEX,
	findRecording,
	type QueueIndex,
} from "@/features/queue/queue-index";
import { taskIDs } from "./upload-coordinator";
import { useUploadCoordinator } from "./use-upload-coordinator";

/**
 * The hook over a fake native module and a fake discovery that sees both
 * Macs: Mac B's `resolve` answers only when the test releases it, and every
 * pinned request and background upload is recorded with its URL and
 * bearer. The real recording client and executor run on top. The pairing,
 * a re-pairing still saving (`replacing`) and the queue are plain state the
 * test drives.
 */
const fake = vi.hoisted(() => {
	const listeners = new Map<string, Set<(event: unknown) => void>>();
	return {
		listeners,
		sent: [] as { url: string; auth: string | undefined }[],
		cancelled: [] as string[],
		releaseMacB: null as (() => void) | null,
		/** When set, the next background upload or device name waits for it. */
		holdUpload: null as Promise<void> | null,
		holdDeviceName: null as Promise<void> | null,
		pairing: null as unknown,
		replacing: null as string | null,
		index: null as unknown,
		update: null as unknown,
		coordinator: null as unknown,
		currentToken: () =>
			fake.replacing ??
			(fake.pairing as { token: string } | null)?.token ??
			null,
		clearIfCurrent: async () => false,
		emit(event: string, payload: unknown) {
			for (const listener of listeners.get(event) ?? []) listener(payload);
		},
	};
});

const HOSTS: Record<string, string> = {
	"Mac A": "10.0.0.1",
	"Mac B": "10.0.0.2",
};

const link = {
	addListener(event: string, listener: (event: unknown) => void) {
		const set = fake.listeners.get(event) ?? new Set();
		set.add(listener);
		fake.listeners.set(event, set);
		return { remove: () => set.delete(listener) };
	},
	resolve(serviceName: string) {
		const mac = { host: HOSTS[serviceName], port: 1 };
		if (serviceName !== "Mac B") return Promise.resolve(mac);
		return new Promise((resolve) => {
			fake.releaseMacB = () => resolve(mac);
		});
	},
	async pendingUploads() {
		return [];
	},
	async startUpload(spec: UploadSpec) {
		fake.sent.push({ url: spec.url, auth: spec.headers.Authorization });
		await fake.holdUpload;
	},
	async cancelUpload(taskID: string) {
		fake.cancelled.push(taskID);
	},
	async request(request: PinnedRequest) {
		fake.sent.push({ url: request.url, auth: request.headers.Authorization });
		if (request.method === "POST") {
			return { status: 200, headers: {}, body: '{"meetingID":"m"}' };
		}
		return {
			status: 200,
			headers: {},
			body: '{"state":"receiving","receivedChunks":[]}',
		};
	},
};

vi.mock("expo", () => ({ requireNativeModule: () => link }));
vi.mock("react-native", () => ({
	AppState: { addEventListener: () => ({ remove() {} }) },
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
		delete() {},
	}),
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
	await act(async () => root?.render(<Probe />));
	await settle();
	return {
		row: (id: string) => findRecording(latest, id),
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

function queued(...ids: string[]) {
	return ids.reduce(
		(index, id) =>
			addRecording(index, {
				recordingID: id,
				fileName: `${id}.m4a`,
				startedAt: "2026-09-25T09:00:00.000Z",
				durationSeconds: 60,
				byteCount: 100,
				sha256: Buffer.alloc(32, 9).toString("base64"),
				chunkSize: 1024,
			}),
		EMPTY_INDEX,
	);
}

function finished(id: string): UploadFinished {
	return { taskID: taskIDs.chunk(id, 0), status: 204, body: "" };
}

function cancelled(id: string): UploadFailed {
	return {
		taskID: taskIDs.chunk(id, 0),
		message: "cancelled",
		retryable: false,
	};
}

beforeEach(() => {
	fake.listeners.clear();
	fake.sent.length = 0;
	fake.cancelled.length = 0;
	fake.releaseMacB = null;
	fake.holdUpload = null;
	fake.holdDeviceName = null;
	fake.replacing = null;
});
afterEach(() => {
	act(() => root?.unmount());
	root = null;
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
	});

	it("sends nothing after Forget Mac", async () => {
		const h = await mount(queued("a"), A);
		await h.repair(null);
		const sentUnderA = fake.sent.length;
		await act(async () => fake.emit("uploadFinished", finished("a")));
		await settle();
		expect(fake.sent.slice(sentUnderA)).toEqual([]);
		expect(h.reachable()).toBe(false);
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
			let release!: () => void;
			fake.holdDeviceName = new Promise((resolve) => {
				release = resolve;
			});
			const h = await mount(queued("a"), A);
			expect(fake.sent).toEqual([]);
			fake.replacing = "token-b";
			await act(async () => release());
			await settle();
			expect(fake.sent).toEqual([]);
			expect(h.row("a")).toMatchObject({
				state: "queued",
				lastError: "Pairing again; retrying",
			});
		});

		it("cancels a chunk whose task was still being created", async () => {
			let release!: () => void;
			fake.holdUpload = new Promise((resolve) => {
				release = resolve;
			});
			await mount(queued("a"), A);
			expect(fake.sent.at(-1)?.url).toBe(
				"https://10.0.0.1:1/v1/recordings/a/chunks/0",
			);
			fake.replacing = "token-b";
			await act(async () => release());
			await settle();
			expect(fake.cancelled).toEqual([taskIDs.chunk("a", 0)]);
		});

		it("goes on under the old pairing when the save fails", async () => {
			vi.useFakeTimers({ shouldAdvanceTime: true, toFake: ["setTimeout"] });
			try {
				const h = await mount(queued("a", "bb"), A);
				fake.replacing = "token-b";
				await act(async () => fake.emit("uploadFailed", cancelled("a")));
				await settle();
				expect(h.row("bb")?.state).toBe("queued");

				fake.replacing = null;
				await act(() => vi.advanceTimersByTimeAsync(1000));
				await settle();
				expect(fake.sent).toContainEqual({
					url: "https://10.0.0.1:1/v1/recordings/bb",
					auth: "Bearer token-a",
				});
			} finally {
				vi.useRealTimers();
			}
		});
	});
});
