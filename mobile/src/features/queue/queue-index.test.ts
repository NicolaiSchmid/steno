import { describe, expect, it } from "vitest";

import {
	addRecording,
	canRetry,
	chunkPlan,
	EMPTY_INDEX,
	findRecording,
	markChunk,
	type NewRecording,
	nextRetryAt,
	nextUploadable,
	patchRecording,
	QueueError,
	type QueueIndex,
	removeRecording,
	resetForUpload,
	type SyncState,
	scheduleRetry,
	setState,
	syncChunks,
	unpairPending,
} from "./queue-index";

const MiB = 1024 * 1024;

function rec(id: string, startedAt: string): NewRecording {
	return {
		recordingID: id,
		fileName: `${id}.m4a`,
		startedAt,
		durationSeconds: 60,
		byteCount: 3 * MiB,
		sha256: "AAAA",
		chunkSize: MiB,
	};
}

const now = new Date("2026-09-25T10:00:00.000Z");

describe("addRecording", () => {
	it("appends a queued row with empty bookkeeping, or a recording row", () => {
		const index = addRecording(
			EMPTY_INDEX,
			rec("a", "2026-09-25T09:00:00.000Z"),
		);
		expect(index.recordings[0]).toMatchObject({
			recordingID: "a",
			state: "queued",
			uploadedChunks: [],
			attempts: 0,
			nextAttemptAt: null,
			lastError: null,
			meetingID: null,
		});
		expect(EMPTY_INDEX.recordings).toHaveLength(0);
		expect(
			addRecording(EMPTY_INDEX, rec("r", "x"), "recording").recordings[0]
				?.state,
		).toBe("recording");
	});

	it("rejects duplicates and bad chunk sizes", () => {
		const index = addRecording(EMPTY_INDEX, rec("a", "x"));
		expect(() => addRecording(index, rec("a", "x"))).toThrow(QueueError);
		expect(() =>
			addRecording(EMPTY_INDEX, { ...rec("b", "x"), chunkSize: 0 }),
		).toThrow(QueueError);
	});
});

describe("state machine", () => {
	/** The plan's state machine, written out independently of the code's table. */
	const LEGAL: Record<SyncState, readonly SyncState[]> = {
		recording: ["queued", "failed"],
		queued: ["uploading", "unpaired", "failed", "delivered"],
		uploading: ["queued", "delivered", "failed", "unpaired"],
		failed: ["queued"],
		// `delivered`: a `complete` sent before the unpair and answered after it.
		unpaired: ["queued", "delivered"],
		delivered: [],
	};
	/** A legal path from `recording` into every state. */
	const PATHS: Record<SyncState, readonly SyncState[]> = {
		recording: [],
		queued: ["queued"],
		uploading: ["queued", "uploading"],
		delivered: ["queued", "uploading", "delivered"],
		failed: ["queued", "failed"],
		unpaired: ["queued", "unpaired"],
	};

	function inState(state: SyncState): QueueIndex {
		return PATHS[state].reduce(
			(index, next) => setState(index, "a", next),
			addRecording(EMPTY_INDEX, rec("a", "x"), "recording"),
		);
	}

	const states = Object.keys(LEGAL) as SyncState[];
	it.each(
		states.flatMap((from) =>
			states.map((to) => ({
				from,
				to,
				legal: from === to || LEGAL[from].includes(to),
			})),
		),
	)("$from -> $to allowed: $legal", ({ from, to, legal }) => {
		const index = inState(from);
		if (legal) {
			expect(setState(index, "a", to).recordings[0]?.state).toBe(to);
		} else {
			expect(() => setState(index, "a", to)).toThrow(
				new RegExp(`illegal transition ${from} -> ${to}`),
			);
			// A rejected transition leaves the input untouched.
			expect(index.recordings[0]?.state).toBe(from);
		}
	});

	it("delivered is terminal for scheduleRetry and resetForUpload too", () => {
		const delivered = inState("delivered");
		expect(() => scheduleRetry(delivered, "a", now, 1, "x")).toThrow(
			QueueError,
		);
		expect(() => resetForUpload(delivered, "a")).toThrow(QueueError);
	});

	it.each(["recording", "queued", "uploading", "failed", "unpaired"] as const)(
		"scheduleRetry lands in queued from %s",
		(from) => {
			expect(
				scheduleRetry(inState(from), "a", now, 1, "x").recordings[0],
			).toMatchObject({ state: "queued", attempts: 1, lastError: "x" });
		},
	);

	it("resetForUpload queues a row with a clean schedule, which may still be delivered", () => {
		const requeued = resetForUpload(
			scheduleRetry(inState("unpaired"), "a", now, 1, "x"),
			"a",
		);
		expect(requeued.recordings[0]).toMatchObject({
			state: "queued",
			attempts: 0,
			nextAttemptAt: null,
			lastError: null,
		});
		// A `complete` answered after a new pairing queued the row again.
		expect(
			setState(requeued, "a", "delivered", { meetingID: "m" }).recordings[0],
		).toMatchObject({ state: "delivered", meetingID: "m" });
	});

	it("treats a same-state call as a patch and never lets the patch change the id or state", () => {
		const patched = setState(inState("queued"), "a", "queued", {
			recordingID: "z",
			state: "delivered",
			lastError: "note",
		} as never);
		expect(patched.recordings[0]).toMatchObject({
			recordingID: "a",
			state: "queued",
			lastError: "note",
		});
	});

	it("throws for unknown ids", () => {
		expect(() => setState(inState("queued"), "nope", "uploading")).toThrow(
			/unknown recording nope/,
		);
		expect(() => removeRecording(inState("queued"), "nope")).toThrow(
			QueueError,
		);
	});
});

describe("unpairPending", () => {
	it("moves queued and uploading rows to unpaired, clears their retry error and leaves the others alone", () => {
		let index = scheduleRetry(
			addRecording(EMPTY_INDEX, rec("q", "1")),
			"q",
			now,
			1,
			"Retrying",
		);
		index = addRecording(index, rec("u", "2"));
		index = setState(index, "u", "uploading");
		index = addRecording(index, rec("d", "3"));
		index = setState(setState(index, "d", "uploading"), "d", "delivered");
		index = addRecording(index, rec("f", "4"));
		index = setState(index, "f", "failed");
		index = addRecording(index, rec("r", "5"), "recording");

		const unpaired = unpairPending(index);
		expect(
			unpaired.recordings.map((r) => [r.recordingID, r.state, r.lastError]),
		).toEqual([
			["q", "unpaired", null],
			["u", "unpaired", null],
			["d", "delivered", null],
			["f", "failed", null],
			["r", "recording", null],
		]);
		expect(nextUploadable(unpaired, now)).toBeNull();
	});

	it("returns the same index when nothing is pending", () => {
		const idle = setState(
			addRecording(EMPTY_INDEX, rec("f", "1")),
			"f",
			"failed",
		);
		expect(unpairPending(idle)).toBe(idle);
	});
});

it("patchRecording fills in the stop-time fields", () => {
	const index = patchRecording(
		addRecording(
			EMPTY_INDEX,
			{ ...rec("a", "x"), sha256: null, byteCount: 0 },
			"recording",
		),
		"a",
		{ sha256: "BBBB", byteCount: 10, durationSeconds: 1.5 },
	);
	expect(index.recordings[0]).toMatchObject({
		sha256: "BBBB",
		byteCount: 10,
		durationSeconds: 1.5,
		state: "recording",
	});
});

describe("markChunk and syncChunks", () => {
	const index = addRecording(EMPTY_INDEX, rec("a", "x"));

	it("keep chunks sorted and unique", () => {
		let next = markChunk(index, "a", 2);
		next = markChunk(next, "a", 0);
		expect(markChunk(next, "a", 2)).toBe(next);
		expect(next.recordings[0]?.uploadedChunks).toEqual([0, 2]);
		const synced = syncChunks(markChunk(index, "a", 1), "a", [2, 0, 2]);
		expect(synced.recordings[0]?.uploadedChunks).toEqual([0, 2]);
	});

	it.each([
		["a", -1],
		["a", 0.5],
		["b", 0],
	])("rejects %s/%s", (id, chunk) => {
		expect(() => markChunk(index, id, chunk)).toThrow(QueueError);
	});
});

describe("chunkPlan", () => {
	it.each([
		[3 * MiB, [MiB, MiB, MiB]],
		[2 * MiB + 5, [MiB, MiB, 5]],
		[5, [5]],
		[0, []],
	])("splits %s bytes", (bytes, lengths) => {
		expect(chunkPlan(bytes, MiB)).toEqual(
			lengths.map((length, index) => ({
				index,
				offset: index * MiB,
				length,
			})),
		);
	});

	it.each([
		[10, 0],
		[-1, 10],
		[1.5, 10],
	])("rejects %s bytes in chunks of %s", (bytes, size) => {
		expect(() => chunkPlan(bytes, size)).toThrow(QueueError);
	});
});

describe("nextUploadable", () => {
	function build(): QueueIndex {
		let index = addRecording(
			EMPTY_INDEX,
			rec("new", "2026-09-25T09:30:00.000Z"),
		);
		index = addRecording(index, rec("old", "2026-09-25T08:00:00.000Z"));
		index = addRecording(index, rec("done", "2026-09-25T07:00:00.000Z"));
		index = setState(setState(index, "done", "uploading"), "done", "delivered");
		index = addRecording(
			index,
			rec("live", "2026-09-25T06:00:00.000Z"),
			"recording",
		);
		// A pending row with no hash never holds up the queue.
		return addRecording(index, {
			...rec("unhashed", "2026-09-25T05:00:00.000Z"),
			sha256: null,
		});
	}

	it("returns the oldest pending recording, skipping delivered, recording and unhashed rows", () => {
		expect(nextUploadable(build(), now)?.recordingID).toBe("old");
		expect(
			nextUploadable(setState(build(), "old", "uploading"), now)?.recordingID,
		).toBe("old");
		expect(nextUploadable(EMPTY_INDEX, now)).toBeNull();
	});

	it("gates on the backoff and reports when the next retry is due", () => {
		let index = scheduleRetry(build(), "old", now, 30_000, "timeout");
		expect(findRecording(index, "old")).toMatchObject({
			state: "queued",
			attempts: 1,
			nextAttemptAt: "2026-09-25T10:00:30.000Z",
			lastError: "timeout",
		});
		expect(nextUploadable(index, now)?.recordingID).toBe("new");
		index = scheduleRetry(index, "new", now, 60_000, "timeout");
		expect(nextUploadable(index, now)).toBeNull();
		expect(nextRetryAt(index, now)).toEqual(
			new Date("2026-09-25T10:00:30.000Z"),
		);
		expect(
			nextUploadable(index, new Date("2026-09-25T10:00:30.000Z"))?.recordingID,
		).toBe("old");
		index = resetForUpload(resetForUpload(index, "old"), "new");
		expect(nextRetryAt(index, now)).toBeNull();
	});
});

it("canRetry is true only for a failed row with a hash", () => {
	const index = setState(
		addRecording(
			addRecording(EMPTY_INDEX, rec("a", "2026-09-25T08:00:00.000Z")),
			{ ...rec("b", "2026-09-25T09:00:00.000Z"), sha256: null },
			"recording",
		),
		"b",
		"failed",
	);
	const failed = setState(index, "a", "failed");
	expect(canRetry(findRecording(failed, "a"))).toBe(true);
	expect(canRetry(findRecording(failed, "b"))).toBe(false);
	expect(canRetry(findRecording(index, "a"))).toBe(false);
	expect(canRetry(null)).toBe(false);
});
