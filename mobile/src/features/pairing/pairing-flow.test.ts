import { describe, expect, it, vi } from "vitest";

import {
	addRecording,
	EMPTY_INDEX,
	findRecording,
	type QueueIndex,
	setState,
} from "@/features/queue/queue-index";
import {
	commitPairing,
	forgetPairing,
	type PairingCommitDependencies,
	type PairingDependencies,
	type PairingForgetDependencies,
	PairingMismatchError,
	performPairing,
} from "./pairing-flow";
import type { PairingPayload } from "./pairing-payload";
import type { Pairing } from "./pairing-store";

const payload: PairingPayload = {
	macID: "0f8fad5b-d9cb-469f-a165-70867728950e",
	macName: "Studio",
	fingerprint: Buffer.alloc(32, 1).toString("base64"),
	secret: Buffer.alloc(32, 2).toString("base64"),
	expiresAt: 1_790_000_000,
};

function deps(
	overrides: Partial<PairingDependencies> = {},
): PairingDependencies {
	return {
		locate: vi.fn(async () => ({ host: "192.168.1.20", port: 51234 })),
		hello: vi.fn(async () => ({
			macID: payload.macID.toUpperCase(),
			protocol: 1,
		})),
		pair: vi.fn(async () => ({
			token: "tok",
			macID: payload.macID,
			macName: "Studio (renamed)",
		})),
		device: vi.fn(async () => ({ deviceID: "dev-1", deviceName: "iPhone" })),
		now: () => new Date("2026-09-25T10:00:00.000Z"),
		...overrides,
	};
}

describe("performPairing", () => {
	it("locates, greets, pairs with the pinned fingerprint and returns the pairing", async () => {
		const d = deps();
		const outcome = await performPairing(payload, d);
		const endpoint = {
			origin: "https://192.168.1.20:51234",
			fingerprint: payload.fingerprint,
		};
		expect(d.locate).toHaveBeenCalledWith(payload.macID);
		expect(d.hello).toHaveBeenCalledWith(endpoint);
		expect(d.pair).toHaveBeenCalledWith(endpoint, payload.secret, {
			deviceID: "dev-1",
			deviceName: "iPhone",
		});
		expect(outcome).toEqual({
			token: "tok",
			mac: {
				macID: payload.macID,
				macName: "Studio (renamed)",
				fingerprint: payload.fingerprint,
				pairedAt: "2026-09-25T10:00:00.000Z",
			},
		});
	});

	it("brackets IPv6 hosts as returned by the module", async () => {
		const d = deps({ locate: async () => ({ host: "[fe80::1]", port: 1 }) });
		await performPairing(payload, d);
		expect(d.hello).toHaveBeenCalledWith({
			origin: "https://[fe80::1]:1",
			fingerprint: payload.fingerprint,
		});
	});

	it("stops before spending the secret when hello names another Mac", async () => {
		const d = deps({ hello: async () => ({ macID: "other", protocol: 1 }) });
		await expect(performPairing(payload, d)).rejects.toBeInstanceOf(
			PairingMismatchError,
		);
		expect(d.pair).not.toHaveBeenCalled();
	});

	it("rejects a pair response from a different identity", async () => {
		const d = deps({
			pair: async () => ({ token: "t", macID: "other", macName: "X" }),
		});
		await expect(performPairing(payload, d)).rejects.toThrow(
			/different identity/,
		);
	});

	it("falls back to the code's name when the Mac sends none", async () => {
		const d = deps({
			pair: async () => ({ token: "t", macID: payload.macID, macName: "" }),
		});
		expect((await performPairing(payload, d)).mac.macName).toBe("Studio");
	});

	it("propagates a failed locate", async () => {
		const d = deps({
			locate: async () => Promise.reject(new Error("not found")),
		});
		await expect(performPairing(payload, d)).rejects.toThrow("not found");
		expect(d.hello).not.toHaveBeenCalled();
	});
});

function recording(recordingID: string) {
	return {
		recordingID,
		fileName: `${recordingID}.m4a`,
		startedAt: "2026-09-25T09:00:00.000Z",
		durationSeconds: 60,
		byteCount: 100,
		sha256: Buffer.alloc(32, 9).toString("base64"),
		chunkSize: 1024,
	};
}

/** One queued recording, "a". */
const one = addRecording(EMPTY_INDEX, recording("a"));

/** Lets every promise callback and the next timer run. */
const macrotask = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("commitPairing", () => {
	const next: Pairing = {
		mac: {
			macID: payload.macID,
			macName: "Studio",
			fingerprint: payload.fingerprint,
			pairedAt: "2026-09-25T10:00:00.000Z",
		},
		token: "tok",
	};

	/**
	 * The save and the cancel resolve only when the test says so; `calls` is
	 * the order. "a" is `unpaired`, "d" was delivered before.
	 */
	function held() {
		const calls: string[] = [];
		const save = Promise.withResolvers<void>();
		const cancel = Promise.withResolvers<void>();
		let index = setState(one, "a", "unpaired");
		index = addRecording(index, recording("d"));
		index = setState(setState(index, "d", "uploading"), "d", "delivered");
		const deps: PairingCommitDependencies = {
			replace: async () => {
				calls.push("replace");
				await save.promise;
			},
			cancelAllUploads: async () => {
				calls.push("cancel");
				await cancel.promise;
			},
			update: async (transform: (index: QueueIndex) => QueueIndex) => {
				calls.push("update");
				index = transform(index);
			},
		};
		const state = (id: string) => findRecording(index, id)?.state;
		return { calls, save, cancel, deps, state };
	}

	it("starts the save, then the cancel, before either settles", async () => {
		const h = held();
		const done = commitPairing(next, h.deps);
		await Promise.resolve();
		expect(h.calls).toEqual(["replace", "cancel"]);

		h.cancel.resolve();
		await macrotask();
		expect(h.calls).toEqual(["replace", "cancel"]);
		h.save.resolve();
		await done;
		expect(h.calls).toEqual(["replace", "cancel", "update"]);
		expect(h.state("a")).toBe("queued");
		expect(h.state("d")).toBe("delivered");
	});

	it("re-queues only once the cancel is done too", async () => {
		const h = held();
		const done = commitPairing(next, h.deps);
		h.save.resolve();
		await macrotask();
		expect(h.calls).toEqual(["replace", "cancel"]);
		h.cancel.resolve();
		await done;
		expect(h.calls).toEqual(["replace", "cancel", "update"]);
	});

	it("re-queues nothing when the save fails", async () => {
		const h = held();
		const done = commitPairing(next, h.deps);
		h.save.reject(new Error("keychain locked"));
		h.cancel.resolve();
		await expect(done).rejects.toThrow("keychain locked");
		expect(h.calls).not.toContain("update");
	});

	it("goes on when the cancel fails", async () => {
		const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
		try {
			const h = held();
			const done = commitPairing(next, h.deps);
			h.cancel.reject(new Error("no session"));
			h.save.resolve();
			await done;
			expect(h.state("a")).toBe("queued");
			expect(warn).toHaveBeenCalledWith(
				"[pairing] cancel failed",
				expect.any(Error),
			);
		} finally {
			warn.mockRestore();
		}
	});
});

describe("forgetPairing", () => {
	/** Each step resolves only when the test says so; `calls` is the order. */
	function held() {
		const calls: string[] = [];
		const clear = Promise.withResolvers<void>();
		const write = Promise.withResolvers<void>();
		const cancel = Promise.withResolvers<void>();
		let index = one;
		const deps: PairingForgetDependencies = {
			clear: async () => {
				calls.push("clear");
				await clear.promise;
			},
			update: async (transform) => {
				calls.push("update");
				await write.promise;
				index = transform(index);
			},
			cancelAllUploads: async () => {
				calls.push("cancel");
				await cancel.promise;
			},
		};
		return { calls, clear, write, cancel, deps, index: () => index };
	}

	it("forgets the pairing, marks the rows unpaired, then cancels the chunks in flight", async () => {
		const h = held();
		const done = forgetPairing(h.deps);
		await macrotask();
		expect(h.calls).toEqual(["clear"]);
		h.clear.resolve();
		await macrotask();
		expect(h.calls).toEqual(["clear", "update"]);
		let settled = false;
		void done.then(() => {
			settled = true;
		});
		h.write.resolve();
		await macrotask();
		expect(h.calls).toEqual(["clear", "update", "cancel"]);
		expect(h.index().recordings[0]?.state).toBe("unpaired");
		expect(settled).toBe(false);
		h.cancel.resolve();
		await done;
	});
});
