import { describe, expect, it } from "vitest";

import {
	addRecording,
	EMPTY_INDEX,
	markChunk,
	type NewRecording,
	scheduleRetry,
	setState,
} from "@/features/queue/queue-index";
import {
	BACKOFF_CAP_MS,
	backoffMs,
	coordinatorStatus,
	parseChunkTaskID,
	planNext,
	taskIDs,
} from "./upload-coordinator";

const MiB = 1024 * 1024;
const now = new Date("2026-09-25T10:00:00.000Z");

function rec(id: string, startedAt: string, byteCount = 3 * MiB): NewRecording {
	return {
		recordingID: id,
		fileName: `${id}.m4a`,
		startedAt,
		durationSeconds: 60,
		byteCount,
		sha256: "AAAA",
		chunkSize: MiB,
	};
}

describe("planNext", () => {
	it("does nothing while the Mac is unreachable, even with work queued", () => {
		const index = addRecording(EMPTY_INDEX, rec("a", "x"));
		expect(planNext(index, false, new Set(), now)).toEqual({ kind: "idle" });
	});

	it("is idle for an empty queue and waits for the next retry otherwise", () => {
		expect(planNext(EMPTY_INDEX, true, new Set(), now)).toEqual({
			kind: "idle",
		});
		const backedOff = scheduleRetry(
			addRecording(EMPTY_INDEX, rec("a", "x")),
			"a",
			now,
			30_000,
			"timeout",
		);
		expect(planNext(backedOff, true, new Set(), now)).toEqual({
			kind: "wait",
			until: "2026-09-25T10:00:30.000Z",
		});
	});

	it("announces before any chunk, once", () => {
		const index = addRecording(EMPTY_INDEX, rec("a", "x"));
		expect(planNext(index, true, new Set(), now)).toEqual({
			kind: "announce",
			recordingID: "a",
		});
		expect(
			planNext(index, true, new Set([taskIDs.announce("a")]), now),
		).toEqual({ kind: "idle" });
	});

	it("skips a recording that has no hash yet", () => {
		const index = addRecording(EMPTY_INDEX, { ...rec("a", "x"), sha256: null });
		expect(planNext(index, true, new Set(), now)).toEqual({
			kind: "idle",
		});
	});

	it("uploads the first missing chunk with at most two in flight", () => {
		const index = setState(
			addRecording(EMPTY_INDEX, rec("a", "x")),
			"a",
			"uploading",
		);
		expect(planNext(index, true, new Set(), now)).toEqual({
			kind: "upload-chunk",
			recordingID: "a",
			chunk: 0,
		});
		const one = new Set([taskIDs.chunk("a", 0)]);
		expect(planNext(index, true, one, now)).toEqual({
			kind: "upload-chunk",
			recordingID: "a",
			chunk: 1,
		});
		const two = new Set([taskIDs.chunk("a", 0), taskIDs.chunk("a", 1)]);
		expect(planNext(index, true, two, now)).toEqual({ kind: "idle" });
	});

	it("resumes from the Mac's chunk set and completes only when nothing is left or in flight", () => {
		let index = setState(
			addRecording(EMPTY_INDEX, rec("a", "x")),
			"a",
			"uploading",
		);
		index = markChunk(markChunk(index, "a", 0), "a", 2);
		expect(planNext(index, true, new Set(), now)).toEqual({
			kind: "upload-chunk",
			recordingID: "a",
			chunk: 1,
		});
		expect(
			planNext(index, true, new Set([taskIDs.chunk("a", 1)]), now),
		).toEqual({
			kind: "idle",
		});
		index = markChunk(index, "a", 1);
		expect(planNext(index, true, new Set(), now)).toEqual({
			kind: "complete",
			recordingID: "a",
		});
		expect(
			planNext(index, true, new Set([taskIDs.complete("a")]), now),
		).toEqual({
			kind: "idle",
		});
	});

	it("works one recording at a time, oldest first", () => {
		let index = addRecording(
			EMPTY_INDEX,
			rec("new", "2026-09-25T09:00:00.000Z"),
		);
		index = addRecording(index, rec("old", "2026-09-25T08:00:00.000Z"));
		index = setState(index, "old", "uploading");
		const busy = new Set([taskIDs.chunk("old", 0), taskIDs.chunk("old", 1)]);
		expect(planNext(index, true, busy, now)).toEqual({ kind: "idle" });
		expect(planNext(index, true, new Set(), now)).toMatchObject({
			recordingID: "old",
		});
	});

	it("treats a delivered or failed recording as done", () => {
		let index = addRecording(EMPTY_INDEX, rec("a", "x"));
		index = setState(setState(index, "a", "uploading"), "a", "delivered");
		expect(planNext(index, true, new Set(), now)).toEqual({
			kind: "idle",
		});
		let failed = addRecording(EMPTY_INDEX, rec("b", "x"));
		failed = setState(failed, "b", "failed");
		expect(planNext(failed, true, new Set(), now)).toEqual({
			kind: "idle",
		});
	});
});

describe("backoffMs", () => {
	it("starts at 5 s, doubles, and caps at 5 min", () => {
		expect(backoffMs(1, () => 1)).toBe(5_000);
		expect(backoffMs(2, () => 1)).toBe(10_000);
		expect(backoffMs(3, () => 1)).toBe(20_000);
		expect(backoffMs(7, () => 1)).toBe(BACKOFF_CAP_MS);
		expect(backoffMs(50, () => 1)).toBe(BACKOFF_CAP_MS);
	});

	it("jitters down to half with the RNG at 0 and treats attempt 0 as the first", () => {
		expect(backoffMs(1, () => 0)).toBe(2_500);
		expect(backoffMs(4, () => 0)).toBe(20_000);
		expect(backoffMs(0, () => 1)).toBe(5_000);
		expect(backoffMs(1, () => 0.5)).toBe(3_750);
	});

	it("clamps a misbehaving RNG", () => {
		expect(backoffMs(1, () => 7)).toBe(5_000);
		expect(backoffMs(1, () => -1)).toBe(2_500);
	});
});

describe("coordinatorStatus", () => {
	const queued = addRecording(EMPTY_INDEX, rec("a", "x"));
	const uploading = setState(queued, "a", "uploading");
	const failed = setState(queued, "a", "failed");
	const delivered = setState(uploading, "a", "delivered");

	it("is unpaired without a pairing, whatever the queue holds", () => {
		expect(coordinatorStatus(false, EMPTY_INDEX, false)).toBe("unpaired");
		expect(coordinatorStatus(false, uploading, true)).toBe("unpaired");
	});

	it("is searching while work is pending and the Mac is not resolved", () => {
		expect(coordinatorStatus(true, queued, false)).toBe("searching");
		expect(coordinatorStatus(true, uploading, false)).toBe("searching");
	});

	it("names the dominant pending state once the Mac is reachable", () => {
		expect(coordinatorStatus(true, queued, true)).toBe("queued");
		expect(coordinatorStatus(true, uploading, true)).toBe("uploading");
		const both = addRecording(uploading, rec("b", "y"));
		expect(coordinatorStatus(true, both, true)).toBe("uploading");
	});

	it("shows failed only when nothing is pending, and idle otherwise", () => {
		expect(coordinatorStatus(true, failed, true)).toBe("failed");
		expect(coordinatorStatus(true, failed, false)).toBe("failed");
		expect(
			coordinatorStatus(true, addRecording(failed, rec("b", "y")), true),
		).toBe("queued");
		expect(coordinatorStatus(true, delivered, true)).toBe("idle");
		expect(coordinatorStatus(true, EMPTY_INDEX, false)).toBe("idle");
		const live = addRecording(EMPTY_INDEX, rec("r", "z"), "recording");
		expect(coordinatorStatus(true, live, true)).toBe("idle");
	});
});

describe("task ids", () => {
	it("round-trips chunk ids and rejects the others", () => {
		expect(parseChunkTaskID(taskIDs.chunk("abc", 3))).toEqual({
			recordingID: "abc",
			chunk: 3,
		});
		expect(parseChunkTaskID(taskIDs.announce("abc"))).toBeNull();
		expect(parseChunkTaskID(taskIDs.complete("abc"))).toBeNull();
		expect(parseChunkTaskID("noslash")).toBeNull();
		expect(parseChunkTaskID("/1")).toBeNull();
	});
});
