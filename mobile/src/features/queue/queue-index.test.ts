import { describe, expect, it } from "vitest";

import {
	addRecording,
	chunkPlan,
	EMPTY_INDEX,
	markChunk,
	type NewRecording,
	nextRetryAt,
	nextUploadable,
	patchRecording,
	QueueError,
	type QueueIndex,
	removeRecording,
	resetForUpload,
	SYNC_STATES,
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
	it("appends a queued row with empty bookkeeping", () => {
		const index = addRecording(
			EMPTY_INDEX,
			rec("a", "2026-09-25T09:00:00.000Z"),
		);
		expect(index.recordings).toHaveLength(1);
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
	});

	it("can start in the recording state and rejects duplicates and bad chunk sizes", () => {
		const index = addRecording(
			EMPTY_INDEX,
			rec("a", "2026-09-25T09:00:00.000Z"),
			"recording",
		);
		expect(index.recordings[0]?.state).toBe("recording");
		expect(() =>
			addRecording(index, rec("a", "2026-09-25T09:00:00.000Z")),
		).toThrow(QueueError);
		expect(() =>
			addRecording(EMPTY_INDEX, { ...rec("b", "x"), chunkSize: 0 }),
		).toThrow(QueueError);
	});
});

describe("state machine", () => {
	const queued = addRecording(
		EMPTY_INDEX,
		rec("a", "2026-09-25T09:00:00.000Z"),
	);

	it("walks recording -> queued -> uploading -> delivered", () => {
		let index = addRecording(EMPTY_INDEX, rec("r", "x"), "recording");
		index = setState(index, "r", "queued");
		index = setState(index, "r", "uploading");
		index = setState(index, "r", "delivered", { meetingID: "m" });
		expect(index.recordings[0]).toMatchObject({
			state: "delivered",
			meetingID: "m",
		});
	});

	it("rejects illegal transitions", () => {
		expect(() => setState(queued, "a", "delivered")).toThrow(
			/illegal transition queued -> delivered/,
		);
		const delivered = setState(
			setState(queued, "a", "uploading"),
			"a",
			"delivered",
		);
		expect(() => setState(delivered, "a", "queued")).toThrow(QueueError);
		expect(() => setState(queued, "a", "recording")).toThrow(QueueError);
	});

	it("allows failed and unpaired back to queued only", () => {
		const failed = setState(queued, "a", "failed", { lastError: "422" });
		expect(() => setState(failed, "a", "uploading")).toThrow(QueueError);
		expect(setState(failed, "a", "queued").recordings[0]?.state).toBe("queued");
		const unpaired = setState(queued, "a", "unpaired");
		expect(() => setState(unpaired, "a", "delivered")).toThrow(QueueError);
		expect(resetForUpload(unpaired, "a").recordings[0]).toMatchObject({
			state: "queued",
			attempts: 0,
			nextAttemptAt: null,
			lastError: null,
		});
	});

	it("treats a same-state call as a patch and never lets the patch change the id or state", () => {
		const patched = setState(queued, "a", "queued", {
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
		expect(() => setState(queued, "nope", "uploading")).toThrow(
			/unknown recording nope/,
		);
		expect(() => removeRecording(queued, "nope")).toThrow(QueueError);
	});
});

describe("every transition", () => {
	/** The plan's state machine, written out independently of the code's table. */
	const LEGAL: Record<SyncState, readonly SyncState[]> = {
		recording: ["queued", "failed"],
		queued: ["uploading", "unpaired", "failed"],
		uploading: ["queued", "delivered", "failed", "unpaired"],
		failed: ["queued"],
		unpaired: ["queued"],
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

	it("lists the six states once", () => {
		expect([...SYNC_STATES].sort()).toEqual(Object.keys(LEGAL).sort());
	});

	for (const from of Object.keys(LEGAL) as SyncState[]) {
		for (const to of Object.keys(LEGAL) as SyncState[]) {
			const legal = from === to || LEGAL[from].includes(to);
			it(`${from} -> ${to} is ${legal ? "allowed" : "rejected"}`, () => {
				const index = inState(from);
				expect(index.recordings[0]?.state).toBe(from);
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
		}
	}

	it("delivered is terminal for scheduleRetry and resetForUpload too", () => {
		const delivered = inState("delivered");
		expect(() => scheduleRetry(delivered, "a", now, 1, "x")).toThrow(
			QueueError,
		);
		expect(() => resetForUpload(delivered, "a")).toThrow(QueueError);
	});

	it("scheduleRetry lands in queued from every state that may still upload", () => {
		for (const from of [
			"recording",
			"queued",
			"uploading",
			"failed",
			"unpaired",
		] as const) {
			expect(
				scheduleRetry(inState(from), "a", now, 1, "x").recordings[0],
			).toMatchObject({ state: "queued", attempts: 1, lastError: "x" });
		}
	});
});

describe("unpairPending", () => {
	it("moves queued and uploading rows to unpaired and leaves the others alone", () => {
		let index = addRecording(EMPTY_INDEX, rec("q", "1"));
		index = addRecording(index, rec("u", "2"));
		index = setState(index, "u", "uploading");
		index = addRecording(index, rec("d", "3"));
		index = setState(setState(index, "d", "uploading"), "d", "delivered");
		index = addRecording(index, rec("f", "4"));
		index = setState(index, "f", "failed");
		index = addRecording(index, rec("r", "5"), "recording");

		const states = Object.fromEntries(
			unpairPending(index).recordings.map((r) => [r.recordingID, r.state]),
		);
		expect(states).toEqual({
			q: "unpaired",
			u: "unpaired",
			d: "delivered",
			f: "failed",
			r: "recording",
		});
		expect(nextUploadable(unpairPending(index), now)).toBeNull();
	});

	it("returns the same index when nothing is pending", () => {
		const idle = setState(
			addRecording(EMPTY_INDEX, rec("f", "1")),
			"f",
			"failed",
		);
		expect(unpairPending(idle)).toBe(idle);
		expect(unpairPending(EMPTY_INDEX)).toBe(EMPTY_INDEX);
	});
});

describe("patchRecording", () => {
	it("fills in the stop-time fields", () => {
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
});

describe("markChunk and syncChunks", () => {
	const index = addRecording(EMPTY_INDEX, rec("a", "x"));

	it("keeps chunks sorted and unique", () => {
		let next = markChunk(index, "a", 2);
		next = markChunk(next, "a", 0);
		next = markChunk(next, "a", 2);
		expect(next.recordings[0]?.uploadedChunks).toEqual([0, 2]);
	});

	it("returns the same index for a duplicate chunk", () => {
		const once = markChunk(index, "a", 1);
		expect(markChunk(once, "a", 1)).toBe(once);
	});

	it("rejects negative or fractional chunks and unknown ids", () => {
		expect(() => markChunk(index, "a", -1)).toThrow(QueueError);
		expect(() => markChunk(index, "a", 0.5)).toThrow(QueueError);
		expect(() => markChunk(index, "b", 0)).toThrow(QueueError);
	});

	it("replaces the set from the Mac's status", () => {
		const synced = syncChunks(markChunk(index, "a", 1), "a", [2, 0, 2]);
		expect(synced.recordings[0]?.uploadedChunks).toEqual([0, 2]);
	});
});

describe("chunkPlan", () => {
	it("splits on exact boundaries", () => {
		expect(chunkPlan(3 * MiB, MiB)).toEqual([
			{ index: 0, offset: 0, length: MiB },
			{ index: 1, offset: MiB, length: MiB },
			{ index: 2, offset: 2 * MiB, length: MiB },
		]);
	});

	it("makes the last chunk short", () => {
		expect(chunkPlan(2 * MiB + 5, MiB)).toEqual([
			{ index: 0, offset: 0, length: MiB },
			{ index: 1, offset: MiB, length: MiB },
			{ index: 2, offset: 2 * MiB, length: 5 },
		]);
		expect(chunkPlan(5, MiB)).toEqual([{ index: 0, offset: 0, length: 5 }]);
	});

	it("yields nothing for an empty file and rejects bad inputs", () => {
		expect(chunkPlan(0, MiB)).toEqual([]);
		expect(() => chunkPlan(10, 0)).toThrow(QueueError);
		expect(() => chunkPlan(-1, 10)).toThrow(QueueError);
		expect(() => chunkPlan(1.5, 10)).toThrow(QueueError);
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
		return index;
	}

	it("returns the oldest pending recording, skipping delivered and recording rows", () => {
		expect(nextUploadable(build(), now)?.recordingID).toBe("old");
	});

	it("gates on the backoff and reports when the next retry is due", () => {
		let index = scheduleRetry(build(), "old", now, 30_000, "timeout");
		expect(index.recordings.find((r) => r.recordingID === "old")).toMatchObject(
			{
				state: "queued",
				attempts: 1,
				nextAttemptAt: "2026-09-25T10:00:30.000Z",
				lastError: "timeout",
			},
		);
		expect(nextUploadable(index, now)?.recordingID).toBe("new");
		index = scheduleRetry(index, "new", now, 60_000, "timeout");
		expect(nextUploadable(index, now)).toBeNull();
		expect(nextRetryAt(index, now)).toEqual(
			new Date("2026-09-25T10:00:30.000Z"),
		);
		expect(
			nextUploadable(index, new Date("2026-09-25T10:00:30.000Z"))?.recordingID,
		).toBe("old");
	});

	it("includes uploading rows and clears the schedule on resetForUpload", () => {
		let index = setState(build(), "old", "uploading");
		expect(nextUploadable(index, now)?.recordingID).toBe("old");
		index = scheduleRetry(index, "old", now, 5_000, "x");
		index = resetForUpload(index, "old");
		expect(nextUploadable(index, now)?.recordingID).toBe("old");
		expect(nextRetryAt(index, now)).toBeNull();
	});

	it("is null for an empty index", () => {
		expect(nextUploadable(EMPTY_INDEX, now)).toBeNull();
	});
});

describe("removeRecording", () => {
	it("drops the row and leaves the rest", () => {
		const index = addRecording(
			addRecording(EMPTY_INDEX, rec("a", "x")),
			rec("b", "y"),
		);
		expect(
			removeRecording(index, "a").recordings.map((r) => r.recordingID),
		).toEqual(["b"]);
	});
});
