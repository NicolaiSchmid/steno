import type { RecordingMetadata, RecordingStatus } from "@modules/steno-link";
import { HandoverError } from "@modules/steno-link/native";
import { describe, expect, it, vi } from "vitest";

import { commitPairing, forgetPairing } from "@/features/pairing/pairing-flow";
import type { Chunk } from "@/features/queue/queue-index";
import {
	addRecording,
	chunkPlan,
	EMPTY_INDEX,
	findRecording,
	type NewRecording,
	type QueueIndex,
	resetForUpload,
	setState,
} from "@/features/queue/queue-index";
import {
	parseQueueIndex,
	serializeQueueIndex,
} from "@/features/queue/queue-storage";
import type { CompleteResult, MacSession } from "./recording-client";
import { planNext, taskIDs } from "./upload-coordinator";
import {
	ANNOUNCE_CONFLICT_MESSAGE,
	ANNOUNCE_CONFLICTS_BEFORE_FAILED,
	createUploadExecutor,
	type ExecutorDependencies,
	type RecordingClient,
} from "./upload-executor";

// `recording-client.ts` reaches the native module lazily; nothing here calls it.
vi.mock("expo", () => ({
	requireNativeModule: () => {
		throw new Error("native module must not be touched by the executor");
	},
}));

const CHUNK = 1024;
const session: MacSession = {
	endpoint: { origin: "https://192.168.1.20:51234", fingerprint: "FP" },
	token: "tok",
};

function rec(id: string, byteCount = 2 * CHUNK + 512): NewRecording {
	return {
		recordingID: id,
		fileName: `${id}.m4a`,
		startedAt: `2026-09-25T09:00:0${id.length}.000Z`,
		durationSeconds: 60,
		byteCount,
		sha256: Buffer.alloc(32, 9).toString("base64"),
		chunkSize: CHUNK,
	};
}

/**
 * A scripted Mac: remembers announced recordings and their received chunks,
 * answers `complete` from that state, and can be revoked, restarted or
 * told to find the file damaged. Chunk bodies never travel: the test
 * delivers the background session's result through `uploadFinished`.
 */
class FakeMac implements RecordingClient {
	readonly calls: string[] = [];
	readonly announced: RecordingMetadata[] = [];
	readonly started: { recordingID: string; chunk: Chunk; uri: string }[] = [];
	readonly received = new Map<string, Set<number>>();
	readonly expected = new Map<string, number>();
	revoked = false;
	damaged = false;
	/** The Mac is still hashing: `complete` answers 409 although every chunk is in. */
	verifying = false;
	/**
	 * Answers for the next announces, in order: 409 (the receipt belongs to
	 * another device) or a transport error; then the Mac answers normally.
	 */
	announceScript: (409 | "unreachable")[] = [];
	/** While set, `complete` is sent but answers only once it resolves. */
	completeGate: Promise<void> | null = null;
	startUploadError: Error | null = null;

	private guard(recordingID: string) {
		if (this.revoked) {
			throw new HandoverError("unauthorized", 401, "revoked");
		}
		if (!this.received.has(recordingID)) {
			throw new HandoverError("not-found", 404, "unknown");
		}
	}

	private statusOf(recordingID: string): RecordingStatus {
		const chunks = [...(this.received.get(recordingID) ?? [])].sort(
			(a, b) => a - b,
		);
		const done = chunks.length === this.expected.get(recordingID);
		return { state: done ? "verifying" : "receiving", receivedChunks: chunks };
	}

	/** Seeds a partial upload the Mac kept from an earlier session. */
	seed(recordingID: string, chunks: number[], expected: number) {
		this.received.set(recordingID, new Set(chunks));
		this.expected.set(recordingID, expected);
	}

	/** The Mac restarted and swept its inbox. */
	forgetAll() {
		this.received.clear();
		this.expected.clear();
	}

	/** Marks a chunk received, as the background session's 204 implies. */
	receive(recordingID: string, chunk: number) {
		this.received.get(recordingID)?.add(chunk);
	}

	announce = async (_: MacSession, metadata: RecordingMetadata) => {
		this.calls.push(`announce ${metadata.recordingID}`);
		if (this.revoked) throw new HandoverError("unauthorized", 401, "revoked");
		const scripted = this.announceScript.shift();
		if (scripted === 409) {
			throw new HandoverError("server", 409, "The Mac answered 409");
		}
		if (scripted === "unreachable") {
			throw new HandoverError("unreachable", null, "Wi-Fi is off");
		}
		this.announced.push(metadata);
		if (!this.received.has(metadata.recordingID)) {
			this.received.set(metadata.recordingID, new Set());
			this.expected.set(
				metadata.recordingID,
				chunkPlan(metadata.byteCount, metadata.chunkSize).length,
			);
		}
		return this.statusOf(metadata.recordingID);
	};

	status = async (_: MacSession, recordingID: string) => {
		this.calls.push(`status ${recordingID}`);
		this.guard(recordingID);
		return this.statusOf(recordingID);
	};

	complete = async (
		_: MacSession,
		recordingID: string,
	): Promise<CompleteResult> => {
		this.calls.push(`complete ${recordingID}`);
		if (this.completeGate) await this.completeGate;
		this.guard(recordingID);
		if (this.statusOf(recordingID).state !== "verifying" || this.verifying) {
			return { kind: "missing-chunks" };
		}
		if (this.damaged) {
			this.received.delete(recordingID);
			return { kind: "hash-mismatch" };
		}
		return { kind: "complete", meetingID: `meeting-${recordingID}` };
	};

	startChunkUpload = async (
		_: MacSession,
		recordingID: string,
		chunk: Chunk,
		uri: string,
	) => {
		this.calls.push(`chunk ${recordingID}/${chunk.index}`);
		if (this.startUploadError) throw this.startUploadError;
		this.started.push({ recordingID, chunk, uri });
	};
}

function harness(initial: QueueIndex) {
	const mac = new FakeMac();
	const state = { index: initial };
	const clock = { now: new Date("2026-09-25T10:00:00.000Z") };
	const files = {
		present: new Set(initial.recordings.map((r) => r.fileName)),
		removed: [] as string[],
	};
	// The phone's pairing, as `PairingProvider.clearIfCurrent` sees it.
	// `loaded` is the token it was loaded with at launch.
	const pairing = {
		token: session.token as string | null,
		loaded: session.token as string | null,
	};
	const onUnauthorized = vi.fn(
		async (token: string | null, unpairRows: () => Promise<unknown>) => {
			const expected = token ?? pairing.loaded;
			if (pairing.token === null || pairing.token !== expected) return false;
			await unpairRows();
			pairing.token = null;
			return true;
		},
	);
	const deps: ExecutorDependencies = {
		client: mac,
		files: {
			exists: (name) => files.present.has(name),
			uri: (name) => `file:///docs/queue/${name}`,
			remove: (name) => {
				files.present.delete(name);
				files.removed.push(name);
			},
		},
		deviceName: async () => "Nicolai's iPhone",
		update: async (transform) => {
			state.index = transform(state.index);
		},
		onUnauthorized,
		now: () => clock.now,
		random: () => 1,
	};
	const executor = createUploadExecutor(deps);

	/** Plans and executes until the planner has nothing to do right now. */
	async function drive(limit = 20) {
		for (let i = 0; i < limit; i++) {
			const action = planNext(state.index, true, executor.inFlight, clock.now);
			if (action.kind === "idle" || action.kind === "wait") return action;
			await executor.execute(action, session, state.index);
		}
		throw new Error("planner did not settle");
	}

	/** The background session reports one chunk done with `status`. */
	async function finishChunk(recordingID: string, chunk: number, status = 204) {
		if (status === 204) mac.receive(recordingID, chunk);
		await executor.uploadFinished({
			taskID: taskIDs.chunk(recordingID, chunk),
			status,
			body: "",
		});
	}

	const row = (id: string) => findRecording(state.index, id);
	const advance = (ms: number) => {
		clock.now = new Date(clock.now.getTime() + ms);
	};

	return {
		mac,
		state,
		clock,
		files,
		executor,
		pairing,
		onUnauthorized,
		drive,
		finishChunk,
		row,
		advance,
	};
}
describe("a recording travels queued -> uploading -> delivered", () => {
	it("announces first, keeps two chunks in flight, completes when all are in, deletes the file and keeps the row", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a")));

		expect(await h.drive()).toEqual({ kind: "idle" });
		expect(h.mac.calls).toEqual(["announce a", "chunk a/0", "chunk a/1"]);
		expect(h.mac.announced[0]).toEqual({
			recordingID: "a",
			startedAt: "2026-09-25T09:00:01.000Z",
			durationSeconds: 60,
			byteCount: 2 * CHUNK + 512,
			sha256: Buffer.alloc(32, 9).toString("base64"),
			chunkSize: CHUNK,
			format: "m4aAAC",
			deviceName: "Nicolai's iPhone",
		});
		expect(h.row("a")).toMatchObject({ state: "uploading", lastError: null });
		expect(h.mac.started.map((s) => s.chunk)).toEqual(
			chunkPlan(2 * CHUNK + 512, CHUNK).slice(0, 2),
		);
		expect(h.mac.started[0]?.uri).toBe("file:///docs/queue/a.m4a");

		await h.finishChunk("a", 0);
		expect(h.row("a")?.uploadedChunks).toEqual([0]);
		expect(await h.drive()).toEqual({ kind: "idle" });
		expect(h.mac.calls.at(-1)).toBe("chunk a/2");

		await h.finishChunk("a", 2);
		// A 200 counts like a 204.
		h.mac.receive("a", 1);
		await h.finishChunk("a", 1, 200);
		expect(h.executor.inFlight.size).toBe(0);
		expect(h.files.removed).toEqual([]);
		expect(await h.drive()).toEqual({ kind: "idle" });
		expect(h.mac.calls.at(-1)).toBe("complete a");

		expect(h.row("a")).toMatchObject({
			state: "delivered",
			meetingID: "meeting-a",
			uploadedChunks: [0, 1, 2],
			lastError: null,
		});
		expect(h.files.removed).toEqual(["a.m4a"]);
		// The delivered row survives a save and load round trip.
		expect(parseQueueIndex(serializeQueueIndex(h.state.index))).toEqual(
			h.state.index,
		);
	});

	it("resumes from the chunks the Mac already has instead of starting at 0", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a")));
		h.mac.seed("a", [0, 2], 3);

		await h.drive();
		expect(h.row("a")?.uploadedChunks).toEqual([0, 2]);
		expect(h.mac.calls).toEqual(["announce a", "chunk a/1"]);

		await h.finishChunk("a", 1);
		await h.drive();
		expect(h.row("a")?.state).toBe("delivered");
		expect(h.mac.started.map((s) => s.chunk.index)).toEqual([1]);
	});

	it("works one recording at a time, oldest first, and moves on after delivery", async () => {
		let index = addRecording(EMPTY_INDEX, rec("bb", CHUNK));
		index = addRecording(index, rec("a", CHUNK));
		const h = harness(index);
		await h.drive();
		expect(h.mac.calls).toEqual(["announce a", "chunk a/0"]);
		await h.finishChunk("a", 0);
		await h.drive();
		expect(h.mac.calls.slice(2)).toEqual([
			"complete a",
			"announce bb",
			"chunk bb/0",
		]);
	});
});

describe("the Mac's answers", () => {
	it("404 on a chunk sends the recording back to queued with no chunks and re-announces after a doubling backoff", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a")));
		await h.drive();
		await h.finishChunk("a", 0);
		h.mac.forgetAll();
		await h.finishChunk("a", 1, 404);

		expect(h.row("a")).toMatchObject({
			state: "queued",
			uploadedChunks: [],
			attempts: 1,
			nextAttemptAt: "2026-09-25T10:00:05.000Z",
			lastError: "The Mac forgot the upload; starting over",
		});
		expect(h.executor.inFlight.has(taskIDs.chunk("a", 1))).toBe(false);

		// Not on the same tick: a Mac that keeps forgetting must not cost a
		// 16 MiB copy per tick.
		const before = h.mac.calls.length;
		expect(await h.drive()).toEqual({
			kind: "wait",
			until: "2026-09-25T10:00:05.000Z",
		});
		expect(h.mac.calls).toHaveLength(before);

		h.advance(5_000);
		await h.drive();
		expect(h.mac.calls.slice(-3)).toEqual([
			"announce a",
			"chunk a/0",
			"chunk a/1",
		]);

		await h.finishChunk("a", 0, 404);
		expect(h.row("a")).toMatchObject({
			attempts: 2,
			nextAttemptAt: "2026-09-25T10:00:15.000Z",
		});
	});

	it("409 on complete re-reads the status and uploads what the Mac lacks", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a", 2 * CHUNK)));
		await h.drive();
		await h.finishChunk("a", 0);
		await h.finishChunk("a", 1);
		// The Mac dropped chunk 1 after acknowledging it.
		h.mac.received.get("a")?.delete(1);

		await h.drive();
		expect(h.mac.calls.slice(-3)).toEqual([
			"complete a",
			"status a",
			"chunk a/1",
		]);
		expect(h.row("a")).toMatchObject({
			state: "uploading",
			uploadedChunks: [0],
		});

		await h.finishChunk("a", 1);
		await h.drive();
		expect(h.row("a")?.state).toBe("delivered");
	});

	it("409 on complete while the Mac already has every chunk backs off instead of looping", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a", CHUNK)));
		await h.drive();
		await h.finishChunk("a", 0);
		h.mac.verifying = true;

		expect(await h.drive()).toEqual({
			kind: "wait",
			until: "2026-09-25T10:00:05.000Z",
		});
		expect(h.mac.calls.slice(-2)).toEqual(["complete a", "status a"]);
		expect(h.row("a")).toMatchObject({
			state: "queued",
			uploadedChunks: [0],
			attempts: 1,
			lastError: "The Mac is not ready to complete the upload",
		});

		// Once the Mac is done verifying, the retry re-announces (200 with the
		// chunk set) and completes without re-uploading anything.
		h.mac.verifying = false;
		h.advance(5_000);
		await h.drive();
		expect(h.mac.calls.slice(-2)).toEqual(["announce a", "complete a"]);
		expect(h.row("a")?.state).toBe("delivered");
		expect(h.mac.started).toHaveLength(1);
	});

	it("422 on complete marks the recording failed, clears its chunks and keeps the file", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a", CHUNK)));
		h.mac.damaged = true;
		await h.drive();
		await h.finishChunk("a", 0);
		expect(await h.drive()).toEqual({ kind: "idle" });

		expect(h.row("a")).toMatchObject({
			state: "failed",
			uploadedChunks: [],
			lastError: "The Mac received a damaged file",
		});
		expect(h.files.removed).toEqual([]);

		// A manual retry announces again from scratch.
		h.mac.damaged = false;
		h.state.index = resetForUpload(h.state.index, "a");
		await h.drive();
		expect(h.mac.calls.slice(-2)).toEqual(["announce a", "chunk a/0"]);
	});

	it("a 5xx on a chunk schedules a retry with the status in the message", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a", CHUNK)));
		await h.drive();
		await h.finishChunk("a", 0, 503);
		expect(h.row("a")).toMatchObject({
			state: "queued",
			attempts: 1,
			nextAttemptAt: "2026-09-25T10:00:05.000Z",
			lastError: "Chunk 0 was answered 503",
		});
	});
});

type Harness = ReturnType<typeof harness>;

describe("401 revokes the pairing", () => {
	function pending() {
		let index = addRecording(EMPTY_INDEX, rec("a", CHUNK));
		index = addRecording(index, rec("bb", CHUNK));
		index = addRecording(index, rec("done", CHUNK));
		index = setState(setState(index, "done", "uploading"), "done", "delivered");
		return addRecording(index, rec("live", CHUNK), "recording");
	}

	it.each<[string, QueueIndex, (h: Harness) => Promise<unknown>]>([
		[
			"at announce",
			pending(),
			async (h) => {
				h.mac.revoked = true;
				await h.drive();
			},
		],
		[
			"on a chunk result",
			pending(),
			async (h) => {
				await h.drive();
				h.mac.revoked = true;
				await h.finishChunk("a", 0, 401);
			},
		],
		[
			"at complete",
			pending(),
			async (h) => {
				await h.drive();
				await h.finishChunk("a", 0);
				h.mac.revoked = true;
				await h.drive();
			},
		],
		[
			"while refreshing the chunk set after a relaunch",
			setState(pending(), "a", "uploading"),
			async (h) => {
				h.mac.revoked = true;
				await h.executor.refreshUploading(session, h.state.index);
			},
		],
		[
			// Nobody recorded the token: it stands for the pairing loaded at launch.
			"on the result of a chunk started before a relaunch",
			setState(pending(), "a", "uploading"),
			async (h) => {
				h.executor.reconcile([taskIDs.chunk("a", 0)]);
				h.mac.revoked = true;
				await h.finishChunk("a", 0, 401);
				expect(h.onUnauthorized).toHaveBeenCalledWith(
					null,
					expect.any(Function),
				);
			},
		],
	])("%s", async (_, index, act) => {
		const h = harness(index);
		await act(h);
		expect(h.row("a")?.state).toBe("unpaired");
		expect(h.row("bb")?.state).toBe("unpaired");
		expect(h.row("done")?.state).toBe("delivered");
		expect(h.row("live")?.state).toBe("recording");
		expect(h.onUnauthorized).toHaveBeenCalledTimes(1);
		expect(h.pairing.token).toBeNull();
		expect(planNext(h.state.index, true, new Set(), h.clock.now)).toEqual({
			kind: "idle",
		});
	});
});

describe("a 401 to a pairing since replaced keeps the new one", () => {
	const twoQueued = () =>
		addRecording(addRecording(EMPTY_INDEX, rec("a", CHUNK)), rec("bb", CHUNK));

	function repaired(h: Harness) {
		h.pairing.token = "new-token";
		h.mac.revoked = true;
	}

	it.each<[string, (h: Harness) => Promise<unknown>]>([
		[
			"at announce",
			async (h) => {
				repaired(h);
				await h.executor.execute(
					{ kind: "announce", recordingID: "a" },
					session,
					h.state.index,
				);
			},
		],
		[
			"on the result of a chunk sent with the old token",
			async (h) => {
				await h.drive();
				repaired(h);
				await h.finishChunk("a", 0, 401);
				expect(h.row("a")?.uploadedChunks).toEqual([]);
			},
		],
		[
			"on the result of a chunk still in flight after a reconcile",
			async (h) => {
				await h.drive();
				h.executor.reconcile([taskIDs.chunk("a", 0)]);
				repaired(h);
				await h.finishChunk("a", 0, 401);
			},
		],
		[
			"at complete",
			async (h) => {
				await h.drive();
				await h.finishChunk("a", 0);
				repaired(h);
				await h.executor.execute(
					{ kind: "complete", recordingID: "a" },
					session,
					h.state.index,
				);
			},
		],
	])("%s retries the row under a neutral message", async (_, act) => {
		const h = harness(twoQueued());
		await act(h);
		expect(h.onUnauthorized).toHaveBeenCalledWith(
			session.token,
			expect.any(Function),
		);
		expect(h.pairing.token).toBe("new-token");
		expect(h.row("a")).toMatchObject({
			state: "queued",
			attempts: 1,
			lastError: "Retrying",
		});
		expect(h.row("a")?.nextAttemptAt).not.toBeNull();
		expect(h.row("bb")?.state).toBe("queued");
	});

	it("while refreshing the chunk sets after a relaunch, and stops at the first", async () => {
		const h = harness(
			setState(setState(twoQueued(), "a", "uploading"), "bb", "uploading"),
		);
		repaired(h);
		await h.executor.refreshUploading(session, h.state.index);
		expect(h.pairing.token).toBe("new-token");
		expect(h.mac.calls).toEqual(["status a"]);
		expect(h.row("a")?.state).toBe("uploading");
		expect(h.row("bb")?.state).toBe("uploading");
	});
});

describe("a complete sent before an unpair and answered after it", () => {
	/**
	 * Uploads every chunk of "a", then sends its `complete` and holds the
	 * answer. While it is held, the user unpairs (`forgetPairing`: the
	 * pairing is cleared and the pending rows become `unpaired`).
	 */
	async function unpairDuringComplete() {
		const h = harness(addRecording(EMPTY_INDEX, rec("a", CHUNK)));
		await h.drive();
		await h.finishChunk("a", 0);
		let answer!: () => void;
		h.mac.completeGate = new Promise((resolve) => {
			answer = resolve;
		});
		const completing = h.executor.execute(
			{ kind: "complete", recordingID: "a" },
			session,
			h.state.index,
		);
		expect(h.mac.calls.at(-1)).toBe("complete a");

		await forgetPairing({
			clear: async () => {
				h.pairing.token = null;
			},
			update: async (transform) => {
				h.state.index = transform(h.state.index);
			},
			cancelAllUploads: async () => {},
		});
		expect(h.row("a")?.state).toBe("unpaired");
		return { h, answer, completing };
	}

	/** Pairs again, as the pairing sheet does after a scan. */
	function pairAgain(h: Harness) {
		return commitPairing(
			{
				mac: {
					macID: "0f8fad5b-d9cb-469f-a165-70867728950e",
					macName: "Studio",
					fingerprint: "FP",
					pairedAt: "2026-09-25T10:05:00.000Z",
				},
				token: "new-token",
			},
			{
				replace: async (pairing) => {
					h.pairing.token = pairing.token;
				},
				update: async (transform) => {
					h.state.index = transform(h.state.index);
				},
				cancelAllUploads: async () => {},
			},
		);
	}

	function expectDelivered(h: Harness) {
		expect(h.row("a")).toMatchObject({
			state: "delivered",
			meetingID: "meeting-a",
			lastError: null,
		});
		expect(h.files.removed).toEqual(["a.m4a"]);
		expect(h.executor.inFlight.size).toBe(0);
	}

	it("a 200 marks the row delivered and deletes the file, and a new pairing does not upload it again", async () => {
		const { h, answer, completing } = await unpairDuringComplete();
		answer();
		await completing;
		expectDelivered(h);

		await pairAgain(h);
		expect(h.row("a")?.state).toBe("delivered");
		const calls = h.mac.calls.length;
		expect(await h.drive()).toEqual({ kind: "idle" });
		expect(h.mac.calls).toHaveLength(calls);
	});

	it("a 401 to that complete leaves the row unpaired with its file, and a new pairing uploads it", async () => {
		const { h, answer, completing } = await unpairDuringComplete();
		// The unpair's revoke reached the Mac before the recording was admitted.
		h.mac.revoked = true;
		answer();
		await completing;

		expect(h.row("a")).toMatchObject({ state: "unpaired", meetingID: null });
		expect(h.files.present.has("a.m4a")).toBe(true);

		await pairAgain(h);
		expect(h.row("a")).toMatchObject({ state: "queued", attempts: 0 });
		// The Mac accepts the new pairing (the fake checks no token).
		h.mac.revoked = false;
		await h.drive();
		expect(h.mac.calls.at(-1)).toBe("complete a");
		expect(h.row("a")?.state).toBe("delivered");
	});

	it("after a new pairing, nothing uploads until the old complete answers, and its 200 delivers the row", async () => {
		const { h, answer, completing } = await unpairDuringComplete();
		await pairAgain(h);
		expect(h.row("a")).toMatchObject({ state: "queued", attempts: 0 });
		const calls = h.mac.calls.length;
		expect(await h.drive()).toEqual({ kind: "idle" });
		expect(h.mac.calls).toHaveLength(calls);

		answer();
		await completing;
		expectDelivered(h);
		expect(await h.drive()).toEqual({ kind: "idle" });
		expect(h.mac.calls).toHaveLength(calls);
	});

	it("after a new pairing, a 401 to the old complete backs off and the new pairing uploads the row", async () => {
		const { h, answer, completing } = await unpairDuringComplete();
		await pairAgain(h);
		// The old token is revoked; the 401 is not the new pairing's.
		h.mac.revoked = true;
		answer();
		await completing;
		expect(h.pairing.token).toBe("new-token");
		expect(h.row("a")).toMatchObject({
			state: "queued",
			attempts: 1,
			lastError: "Retrying",
		});
		expect(h.files.present.has("a.m4a")).toBe(true);

		h.mac.revoked = false;
		h.advance(5_000);
		await h.drive();
		expect(h.mac.calls.slice(-2)).toEqual(["announce a", "complete a"]);
		expect(h.row("a")?.state).toBe("delivered");
	});
});

describe("an announce the Mac keeps refusing with 409", () => {
	it("fails the row after three in a row, and Retry announces again", async () => {
		expect(ANNOUNCE_CONFLICTS_BEFORE_FAILED).toBe(3);
		const h = harness(addRecording(EMPTY_INDEX, rec("a", CHUNK)));
		h.mac.announceScript = [409, 409, 409];

		expect(await h.drive()).toMatchObject({ kind: "wait" });
		expect(h.row("a")).toMatchObject({
			state: "queued",
			attempts: 1,
			lastError: "The Mac answered 409",
		});
		h.advance(60_000);
		expect(await h.drive()).toMatchObject({ kind: "wait" });
		h.advance(60_000);
		expect(await h.drive()).toEqual({ kind: "idle" });
		expect(h.row("a")).toMatchObject({
			state: "failed",
			lastError: ANNOUNCE_CONFLICT_MESSAGE,
		});
		// Nothing is retried behind the user's back.
		h.advance(60 * 60_000);
		expect(await h.drive()).toEqual({ kind: "idle" });
		expect(h.mac.calls).toEqual(["announce a", "announce a", "announce a"]);
		expect(h.files.present.has("a.m4a")).toBe(true);

		// Retry: the count starts again, so one more 409 only retries, and
		// then the Mac takes it.
		h.mac.announceScript = [409];
		h.state.index = resetForUpload(h.state.index, "a");
		expect(await h.drive()).toMatchObject({ kind: "wait" });
		expect(h.row("a")).toMatchObject({ state: "queued", attempts: 1 });
		h.advance(60_000);
		await h.drive();
		expect(h.row("a")).toMatchObject({ state: "uploading", lastError: null });
	});

	it("starts counting again after an announce the Mac takes", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a", CHUNK)));
		h.mac.announceScript = [409, 409];
		for (let i = 0; i < 2; i++) {
			expect(await h.drive()).toMatchObject({ kind: "wait" });
			h.advance(60 * 60_000);
		}
		// The third announce is taken; the chunk cannot start, so the row
		// goes back to `queued` and announces again.
		h.mac.startUploadError = new Error("no disk");
		expect(await h.drive()).toMatchObject({ kind: "wait" });
		expect(h.mac.calls.slice(-2)).toEqual(["announce a", "chunk a/0"]);
		h.mac.startUploadError = null;
		h.mac.announceScript = [409];
		h.advance(60 * 60_000);
		expect(await h.drive()).toMatchObject({ kind: "wait" });
		expect(h.row("a")).toMatchObject({ state: "queued", attempts: 4 });
	});

	it("counts only 409s in a row", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a", CHUNK)));
		h.mac.announceScript = [409, 409, "unreachable", 409, 409];
		for (let i = 0; i < 5; i++) {
			expect(await h.drive()).toMatchObject({ kind: "wait" });
			expect(h.row("a")?.state).toBe("queued");
			h.advance(60 * 60_000);
		}
		await h.drive();
		expect(h.row("a")?.state).toBe("uploading");
	});
});

describe("transient failures back off on the injected clock", () => {
	it("doubles from 5 s and the planner waits until the next attempt", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a", CHUNK)));
		h.mac.announceScript = ["unreachable", "unreachable", "unreachable"];

		expect(await h.drive()).toEqual({
			kind: "wait",
			until: "2026-09-25T10:00:05.000Z",
		});
		expect(h.row("a")).toMatchObject({
			state: "queued",
			attempts: 1,
			lastError: "Wi-Fi is off",
		});

		h.advance(4_999);
		expect(await h.drive()).toMatchObject({ kind: "wait" });
		expect(h.mac.calls).toEqual(["announce a"]);

		h.advance(1);
		expect(await h.drive()).toEqual({
			kind: "wait",
			until: "2026-09-25T10:00:15.000Z",
		});
		h.advance(10_000);
		expect(await h.drive()).toEqual({
			kind: "wait",
			until: "2026-09-25T10:00:35.000Z",
		});

		h.advance(20_000);
		await h.drive();
		expect(h.mac.calls.slice(-2)).toEqual(["announce a", "chunk a/0"]);
		expect(h.row("a")).toMatchObject({
			state: "uploading",
			attempts: 3,
			lastError: null,
		});
	});

	it.each([
		[
			"a cancelled task (a rejected pin)",
			false,
			"The Mac's certificate does not match the pairing",
		],
		["a retryable task", true, "The network connection was lost."],
	])(
		"every background failure schedules a retry: %s",
		async (_, retryable, message) => {
			const h = harness(addRecording(EMPTY_INDEX, rec("a", CHUNK)));
			await h.drive();
			await h.executor.uploadFailed({
				taskID: taskIDs.chunk("a", 0),
				message,
				retryable,
			});
			expect(h.executor.inFlight.size).toBe(0);
			expect(h.row("a")).toMatchObject({
				state: "queued",
				attempts: 1,
				nextAttemptAt: "2026-09-25T10:00:05.000Z",
				lastError: message,
			});
			// Re-planning at once would copy and hand off the chunk again.
			expect(await h.drive()).toMatchObject({ kind: "wait" });
			expect(h.mac.started).toHaveLength(1);
		},
	);

	it("a failed hand-off to the background session frees the slot and retries", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a", CHUNK)));
		h.mac.startUploadError = new Error("ERR_STENO_UPLOAD");
		expect(await h.drive()).toMatchObject({ kind: "wait" });
		expect(h.executor.inFlight.size).toBe(0);
		expect(h.row("a")).toMatchObject({
			state: "queued",
			attempts: 1,
			lastError: "ERR_STENO_UPLOAD",
		});
	});
});

it("fails a recording whose file is gone without calling the Mac", async () => {
	const h = harness(addRecording(EMPTY_INDEX, rec("a", CHUNK)));
	h.files.present.clear();
	await h.drive();
	expect(h.mac.calls).toEqual([]);
	expect(h.row("a")).toMatchObject({
		state: "failed",
		lastError: "The recording file is missing",
	});
});

it("reconcile drops chunk ids the session no longer knows, adopts the ones it does, keeps announce and complete ids", async () => {
	const h = harness(addRecording(EMPTY_INDEX, rec("a")));
	await h.drive();
	h.executor.inFlight.add(taskIDs.announce("bb"));
	h.executor.inFlight.add(taskIDs.complete("cc"));

	// Chunk 0's `uploadFinished` was missed (dev reload); chunk 2 was
	// started by a previous JS lifetime.
	h.executor.reconcile([taskIDs.chunk("a", 1), taskIDs.chunk("a", 2)]);
	expect(h.executor.inFlight).toEqual(
		new Set([
			taskIDs.chunk("a", 1),
			taskIDs.chunk("a", 2),
			taskIDs.announce("bb"),
			taskIDs.complete("cc"),
		]),
	);

	// Once chunk 2 finishes natively, the planner no longer waits on the
	// dropped chunk 0.
	h.executor.reconcile([taskIDs.chunk("a", 1)]);
	expect(
		planNext(h.state.index, true, h.executor.inFlight, h.clock.now),
	).toEqual({ kind: "upload-chunk", recordingID: "a", chunk: 0 });
});

it("refreshUploading after a relaunch syncs every uploading row, requeues one the Mac forgot and skips the others", async () => {
	let index = addRecording(EMPTY_INDEX, rec("a"));
	index = addRecording(index, rec("bb"));
	index = addRecording(index, rec("ccc"));
	index = setState(setState(index, "a", "uploading"), "ccc", "uploading");
	const h = harness(index);
	h.mac.seed("a", [1], 3);
	await h.executor.refreshUploading(session, h.state.index);
	expect(h.mac.calls).toEqual(["status a", "status ccc"]);
	expect(h.row("a")?.uploadedChunks).toEqual([1]);
	expect(h.row("bb")?.state).toBe("queued");
	expect(h.row("ccc")).toMatchObject({ state: "queued", uploadedChunks: [] });
});
