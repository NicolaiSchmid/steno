import type { RecordingMetadata, RecordingStatus } from "@modules/steno-link";
import { describe, expect, it, vi } from "vitest";

import { HandoverError } from "@/features/pairing/pairing-client";
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
import type { CompleteResult, Session } from "./recording-client";
import { planNext, taskIDs } from "./upload-coordinator";
import {
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
const session: Session = {
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
	/** Announces that fail with a transport error before succeeding. */
	unreachableAnnounces = 0;
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

	announce = async (_: Session, metadata: RecordingMetadata) => {
		this.calls.push(`announce ${metadata.recordingID}`);
		if (this.revoked) throw new HandoverError("unauthorized", 401, "revoked");
		if (this.unreachableAnnounces > 0) {
			this.unreachableAnnounces -= 1;
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

	status = async (_: Session, recordingID: string) => {
		this.calls.push(`status ${recordingID}`);
		this.guard(recordingID);
		return this.statusOf(recordingID);
	};

	complete = async (
		_: Session,
		recordingID: string,
	): Promise<CompleteResult> => {
		this.calls.push(`complete ${recordingID}`);
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
		_: Session,
		recordingID: string,
		chunk: Chunk,
		uri: string,
	) => {
		this.calls.push(`chunk ${recordingID}/${chunk.index}`);
		if (this.startUploadError) throw this.startUploadError;
		this.started.push({ recordingID, chunk, uri });
	};
}

function harness(initial: QueueIndex, random = () => 1) {
	const mac = new FakeMac();
	const state = { index: initial, saves: 0 };
	const clock = { now: new Date("2026-09-25T10:00:00.000Z") };
	const files = {
		present: new Set(initial.recordings.map((r) => r.fileName)),
		removed: [] as string[],
	};
	const onUnauthorized = vi.fn(async () => {});
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
			const next = transform(state.index);
			if (next !== state.index) state.saves += 1;
			state.index = next;
		},
		onUnauthorized,
		now: () => clock.now,
		random,
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
		expect(h.row("a")).toMatchObject({ state: "uploading", lastError: null });
		expect(h.executor.inFlight).toEqual(
			new Set([taskIDs.chunk("a", 0), taskIDs.chunk("a", 1)]),
		);
		expect(h.mac.started.map((s) => s.chunk)).toEqual(
			chunkPlan(2 * CHUNK + 512, CHUNK).slice(0, 2),
		);
		expect(h.mac.started[0]?.uri).toBe("file:///docs/queue/a.m4a");

		await h.finishChunk("a", 0);
		expect(h.row("a")?.uploadedChunks).toEqual([0]);
		expect(await h.drive()).toEqual({ kind: "idle" });
		expect(h.mac.calls.at(-1)).toBe("chunk a/2");

		await h.finishChunk("a", 2);
		await h.finishChunk("a", 1);
		expect(h.executor.inFlight.size).toBe(0);
		expect(await h.drive()).toEqual({ kind: "idle" });
		expect(h.mac.calls.at(-1)).toBe("complete a");

		expect(h.row("a")).toMatchObject({
			state: "delivered",
			meetingID: "meeting-a",
			uploadedChunks: [0, 1, 2],
			lastError: null,
		});
		expect(h.files.removed).toEqual(["a.m4a"]);
		expect(h.state.index.recordings).toHaveLength(1);
		// The delivered row survives a save and load round trip.
		expect(parseQueueIndex(serializeQueueIndex(h.state.index))).toEqual(
			h.state.index,
		);
	});

	it("announces core's RecordingMetadata with the device name and m4aAAC", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a")));
		await h.drive();
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
	});

	it("resumes from the chunks the Mac already has instead of starting at 0", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a")));
		h.mac.seed("a", [0, 2], 3);

		await h.drive();
		expect(h.row("a")?.uploadedChunks).toEqual([0, 2]);
		expect(h.mac.calls).toEqual(["announce a", "chunk a/1"]);

		await h.finishChunk("a", 1);
		await h.drive();
		expect(h.mac.calls.at(-1)).toBe("complete a");
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
	it("404 on a chunk sends the recording back to queued with no chunks and re-announces after the backoff", async () => {
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
	});

	it("a second 404 doubles the wait", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a", CHUNK)));
		await h.drive();
		await h.finishChunk("a", 0, 404);
		h.advance(5_000);
		await h.drive();
		await h.finishChunk("a", 0, 404);
		expect(h.row("a")).toMatchObject({
			attempts: 2,
			nextAttemptAt: "2026-09-25T10:00:15.000Z",
		});
	});

	it("404 on a chunk of a row that is no longer uploading changes nothing", async () => {
		const failed = setState(addRecording(EMPTY_INDEX, rec("a")), "a", "failed");
		const h = harness(failed);
		await h.finishChunk("a", 0, 404);
		expect(h.state.index).toBe(failed);
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

	it("a 200 on a chunk counts like a 204", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a", CHUNK)));
		await h.drive();
		await h.finishChunk("a", 0, 200);
		expect(h.row("a")?.uploadedChunks).toEqual([0]);
	});
});

describe("401 revokes the pairing", () => {
	function twoPending() {
		let index = addRecording(EMPTY_INDEX, rec("a", CHUNK));
		index = addRecording(index, rec("bb", CHUNK));
		index = addRecording(index, rec("done", CHUNK));
		index = setState(setState(index, "done", "uploading"), "done", "delivered");
		index = addRecording(index, rec("live", CHUNK), "recording");
		return index;
	}

	function expectRevoked(h: ReturnType<typeof harness>) {
		expect(h.row("a")?.state).toBe("unpaired");
		expect(h.row("bb")?.state).toBe("unpaired");
		expect(h.row("done")?.state).toBe("delivered");
		expect(h.row("live")?.state).toBe("recording");
		expect(h.onUnauthorized).toHaveBeenCalledTimes(1);
	}

	it("at announce", async () => {
		const h = harness(twoPending());
		h.mac.revoked = true;
		await h.drive();
		expectRevoked(h);
		expect(h.mac.calls).toEqual(["announce a"]);
	});

	it("on a chunk result", async () => {
		const h = harness(twoPending());
		await h.drive();
		h.mac.revoked = true;
		await h.finishChunk("a", 0, 401);
		expectRevoked(h);
	});

	it("at complete", async () => {
		const h = harness(twoPending());
		await h.drive();
		await h.finishChunk("a", 0);
		h.mac.revoked = true;
		await h.drive();
		expectRevoked(h);
	});

	it("while refreshing the chunk set after a relaunch", async () => {
		const h = harness(setState(twoPending(), "a", "uploading"));
		h.mac.revoked = true;
		await h.executor.refreshUploading(session, h.state.index);
		expectRevoked(h);
	});

	it("leaves nothing pending for the planner afterwards", async () => {
		const h = harness(twoPending());
		h.mac.revoked = true;
		await h.drive();
		expect(planNext(h.state.index, true, new Set(), h.clock.now)).toEqual({
			kind: "idle",
		});
	});
});

describe("transient failures back off on the injected clock", () => {
	it("doubles from 5 s with the RNG at 1 and the planner waits until the next attempt", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a", CHUNK)));
		h.mac.unreachableAnnounces = 3;

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
		expect(h.row("a")?.attempts).toBe(2);

		h.advance(10_000);
		expect(await h.drive()).toEqual({
			kind: "wait",
			until: "2026-09-25T10:00:35.000Z",
		});
		expect(h.row("a")?.attempts).toBe(3);

		h.advance(20_000);
		await h.drive();
		expect(h.mac.calls).toEqual([
			"announce a",
			"announce a",
			"announce a",
			"announce a",
			"chunk a/0",
		]);
		expect(h.row("a")).toMatchObject({
			state: "uploading",
			attempts: 3,
			lastError: null,
		});
	});

	it("halves the delay with the RNG at 0", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a", CHUNK)), () => 0);
		h.mac.unreachableAnnounces = 2;
		expect(await h.drive()).toEqual({
			kind: "wait",
			until: "2026-09-25T10:00:02.500Z",
		});
		h.advance(2_500);
		expect(await h.drive()).toEqual({
			kind: "wait",
			until: "2026-09-25T10:00:07.500Z",
		});
	});

	it("every background failure schedules a retry, a cancelled task included", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a", CHUNK)));
		await h.drive();
		// A rejected pin surfaces as a cancellation; re-planning it at once
		// would copy and hand off the chunk again on the same tick.
		await h.executor.uploadFailed({
			taskID: taskIDs.chunk("a", 0),
			message: "The Mac's certificate does not match the pairing",
			retryable: false,
		});
		expect(h.executor.inFlight.has(taskIDs.chunk("a", 0))).toBe(false);
		expect(h.row("a")).toMatchObject({
			state: "queued",
			attempts: 1,
			nextAttemptAt: "2026-09-25T10:00:05.000Z",
			lastError: "The Mac's certificate does not match the pairing",
		});
		expect(await h.drive()).toEqual({
			kind: "wait",
			until: "2026-09-25T10:00:05.000Z",
		});
		expect(h.mac.started).toHaveLength(1);

		h.advance(5_000);
		await h.drive();
		await h.executor.uploadFailed({
			taskID: taskIDs.chunk("a", 0),
			message: "The network connection was lost.",
			retryable: true,
		});
		expect(h.row("a")).toMatchObject({
			state: "queued",
			attempts: 2,
			lastError: "The network connection was lost.",
		});
	});

	it("ignores results for task ids that are not chunks", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a", CHUNK)));
		const before = h.state.index;
		h.executor.inFlight.add("stale/announce");
		await h.executor.uploadFinished({
			taskID: "stale/announce",
			status: 500,
			body: "",
		});
		await h.executor.uploadFailed({
			taskID: "nonsense",
			message: "x",
			retryable: true,
		});
		expect(h.executor.inFlight.size).toBe(0);
		expect(h.state.index).toBe(before);
	});

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

	it("does not retry a row that stopped being pending in the meantime", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a", CHUNK)));
		h.state.index = setState(h.state.index, "a", "failed");
		await h.executor.fail("a", new Error("late"));
		expect(h.row("a")).toMatchObject({ state: "failed", attempts: 0 });
	});
});

describe("guards", () => {
	it("fails a recording whose file is gone without calling the Mac", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a", CHUNK)));
		h.files.present.clear();
		await h.drive();
		expect(h.mac.calls).toEqual([]);
		expect(h.row("a")).toMatchObject({
			state: "failed",
			lastError: "The recording file is missing",
		});
		expect(h.executor.inFlight.size).toBe(0);
	});

	it("ignores actions for unknown recordings or chunks beyond the plan", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a", CHUNK)));
		const before = h.state.index;
		await h.executor.execute(
			{ kind: "announce", recordingID: "ghost" },
			session,
			h.state.index,
		);
		await h.executor.execute(
			{ kind: "upload-chunk", recordingID: "a", chunk: 7 },
			session,
			h.state.index,
		);
		await h.executor.execute({ kind: "idle" }, session, h.state.index);
		await h.executor.execute(
			{ kind: "wait", until: "2026-09-25T10:00:05.000Z" },
			session,
			h.state.index,
		);
		expect(h.mac.calls).toEqual([]);
		expect(h.state.index).toBe(before);
		expect(h.executor.inFlight.size).toBe(0);
	});
});

describe("reconcile with the background session", () => {
	it("drops chunk ids the session no longer knows, adopts the ones it does, keeps announce and complete ids", async () => {
		const h = harness(addRecording(EMPTY_INDEX, rec("a")));
		await h.drive();
		expect(h.executor.inFlight).toEqual(
			new Set([taskIDs.chunk("a", 0), taskIDs.chunk("a", 1)]),
		);
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
		).toEqual({
			kind: "upload-chunk",
			recordingID: "a",
			chunk: 0,
		});
	});
});

describe("refreshUploading after a relaunch", () => {
	it("syncs the chunk set of every uploading row and skips the others", async () => {
		let index = addRecording(EMPTY_INDEX, rec("a"));
		index = addRecording(index, rec("bb"));
		index = setState(index, "a", "uploading");
		const h = harness(index);
		h.mac.seed("a", [1], 3);
		await h.executor.refreshUploading(session, h.state.index);
		expect(h.mac.calls).toEqual(["status a"]);
		expect(h.row("a")?.uploadedChunks).toEqual([1]);
		expect(h.row("bb")?.state).toBe("queued");
	});

	it("sends a row the Mac no longer knows back to queued", async () => {
		const index = setState(
			addRecording(EMPTY_INDEX, rec("a")),
			"a",
			"uploading",
		);
		const h = harness(index);
		await h.executor.refreshUploading(session, h.state.index);
		expect(h.row("a")).toMatchObject({ state: "queued", uploadedChunks: [] });
	});

	it("stops when cancelled", async () => {
		let index = setState(addRecording(EMPTY_INDEX, rec("a")), "a", "uploading");
		index = setState(addRecording(index, rec("bb")), "bb", "uploading");
		const h = harness(index);
		h.mac.seed("a", [], 3);
		h.mac.seed("bb", [], 3);
		let calls = 0;
		await h.executor.refreshUploading(
			session,
			h.state.index,
			() => calls++ > 0,
		);
		expect(h.mac.calls).toEqual(["status a"]);
	});
});
