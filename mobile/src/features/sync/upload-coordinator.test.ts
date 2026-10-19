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

	it("does not announce a queued recording while its complete is still out", () => {
		const index = addRecording(EMPTY_INDEX, rec("a", "x"));
		expect(
			planNext(index, true, new Set([taskIDs.complete("a")]), now),
		).toEqual({ kind: "idle" });
	});

	it("skips a recording with no hash, so it never holds up a newer one", () => {
		const hashless = addRecording(EMPTY_INDEX, {
			...rec("a", "2026-09-25T08:00:00.000Z"),
			sha256: null,
		});
		expect(planNext(hashless, true, new Set(), now)).toEqual({
			kind: "idle",
		});
		const index = addRecording(hashless, rec("b", "2026-09-25T09:00:00.000Z"));
		expect(planNext(index, true, new Set(), now)).toEqual({
			kind: "announce",
			recordingID: "b",
		});
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
});

describe("backoffMs", () => {
	it.each([
		[1, 1, 5_000],
		[2, 1, 10_000],
		[3, 1, 20_000],
		[7, 1, BACKOFF_CAP_MS],
		[50, 1, BACKOFF_CAP_MS],
		// Attempt 0 counts as the first; the RNG jitters down to half.
		[0, 1, 5_000],
		[1, 0, 2_500],
		[4, 0, 20_000],
		[1, 0.5, 3_750],
	])("attempt %s with the RNG at %s waits %s ms", (attempt, random, ms) => {
		expect(backoffMs(attempt, () => random)).toBe(ms);
	});
});

describe("coordinatorStatus", () => {
	const queued = addRecording(EMPTY_INDEX, rec("a", "x"));
	const uploading = setState(queued, "a", "uploading");
	const failed = setState(queued, "a", "failed");
	const delivered = setState(uploading, "a", "delivered");
	const live = addRecording(EMPTY_INDEX, rec("r", "z"), "recording");

	it.each([
		["no pairing", false, EMPTY_INDEX, false, "unpaired"],
		["no pairing, uploading", false, uploading, true, "unpaired"],
		["queued, Mac not resolved", true, queued, false, "searching"],
		["uploading, Mac not resolved", true, uploading, false, "searching"],
		["queued", true, queued, true, "queued"],
		["uploading", true, uploading, true, "uploading"],
		[
			"uploading and queued",
			true,
			addRecording(uploading, rec("b", "y")),
			true,
			"uploading",
		],
		["failed", true, failed, true, "failed"],
		["failed, Mac not resolved", true, failed, false, "failed"],
		[
			"failed and queued",
			true,
			addRecording(failed, rec("b", "y")),
			true,
			"queued",
		],
		["delivered", true, delivered, true, "idle"],
		["empty, Mac not resolved", true, EMPTY_INDEX, false, "idle"],
		["recording", true, live, true, "idle"],
	] as const)("%s", (_, paired, index, reachable, expected) => {
		expect(coordinatorStatus(paired, index, reachable)).toBe(expected);
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
