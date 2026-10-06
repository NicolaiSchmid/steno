// @vitest-environment happy-dom
import type {
	PinnedRequest,
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
 * The hook over a fake native module: Bonjour sees both Macs, Mac B's
 * `resolve` answers only when the test releases it, and every pinned
 * request and background upload is recorded with its URL and bearer. The
 * real recording client and executor run on top. The pairing and the queue
 * are plain state the test drives.
 */
const fake = vi.hoisted(() => {
	const listeners = new Map<string, Set<(event: unknown) => void>>();
	return {
		listeners,
		/** Every pinned request and background upload, in order. */
		sent: [] as { url: string; auth: string | undefined }[],
		releaseMacB: null as (() => void) | null,
		pairing: null as unknown,
		index: null as unknown,
		update: null as unknown,
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
		clearIfCurrent: fake.clearIfCurrent,
	}),
}));
vi.mock("@/features/pairing/pairing-store", () => ({
	deviceIdentity: async () => ({ deviceName: "Phone" }),
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
	let setPairing!: (pairing: Pairing) => void;
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
		const [current, setCurrent] = useState(pairing);
		const [index, setIndexState] = useState(initial);
		setPairing = setCurrent;
		setIndex = setIndexState;
		fake.pairing = current;
		fake.index = index;
		useUploadCoordinator();
		return null;
	}
	root = createRoot(document.createElement("div"));
	await act(async () => root?.render(<Probe />));
	await settle();
	return {
		row: (id: string) => findRecording(latest, id),
		repair: (next: Pairing) => act(async () => setPairing(next)),
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

beforeEach(() => {
	fake.listeners.clear();
	fake.sent.length = 0;
	fake.releaseMacB = null;
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
		const before = fake.sent.length;
		// The chunk sent under A lands, which would plan `complete` next.
		await act(async () =>
			fake.emit("uploadFinished", {
				taskID: taskIDs.chunk("a", 0),
				status: 204,
				body: "",
			} satisfies UploadFinished),
		);
		await settle();
		expect(fake.sent.slice(before)).toEqual([]);

		await act(async () => fake.releaseMacB?.());
		await settle();
		const after = fake.sent.slice(before);
		expect(after).toContainEqual({
			url: "https://10.0.0.2:1/v1/recordings/a/complete",
			auth: "Bearer token-b",
		});
		for (const request of after) {
			expect(request.url.startsWith("https://10.0.0.2:1/")).toBe(true);
			expect(request.auth).toBe("Bearer token-b");
		}
	});
});
