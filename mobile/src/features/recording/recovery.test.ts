import { describe, expect, it } from "vitest";

import {
	addRecording,
	EMPTY_INDEX,
	setState,
} from "@/features/queue/queue-index";
import { applyRecovery, planRecovery, type RecoveryFiles } from "./recovery";

const SOURCE = "file:///docs/ExpoAudio/recording-123.m4a";

function files(overrides: Partial<RecoveryFiles> = {}): RecoveryFiles {
	return {
		size: () => 80_000,
		adopt: async () => {},
		sha256: async () => "HASH",
		...overrides,
	};
}

/** Files keyed by URI or queue file name; `adopt` moves between them. */
function disk(initial: Record<string, number>) {
	const sizes = new Map(Object.entries(initial));
	const moves: [string, string][] = [];
	const api: RecoveryFiles = {
		size: (fileName) => sizes.get(fileName) ?? 0,
		adopt: async (sourceUri, fileName) => {
			const size = sizes.get(sourceUri);
			if (size === undefined) throw new Error(`missing ${sourceUri}`);
			sizes.delete(sourceUri);
			sizes.set(fileName, size);
			moves.push([sourceUri, fileName]);
		},
		sha256: async (fileName) => `sha(${fileName})`,
	};
	return { api, moves, sizes };
}

const base = {
	fileName: "a.m4a",
	startedAt: "2026-09-25T09:00:00.000Z",
	durationSeconds: 0,
	byteCount: 0,
	sha256: null,
	chunkSize: 16,
};

/** Left in `recording` by a crash; the recorder was writing to `SOURCE`. */
const interrupted = addRecording(
	EMPTY_INDEX,
	{ ...base, recordingID: "a", sourceUri: SOURCE },
	"recording",
);

describe("planRecovery", () => {
	it("moves the recorder's file into the queue directory, then hashes and queues it", async () => {
		const d = disk({ [SOURCE]: 80_000 });
		expect(await planRecovery(interrupted, d.api)).toEqual([
			{
				recordingID: "a",
				kind: "queued",
				byteCount: 80_000,
				sha256: "sha(a.m4a)",
				durationSeconds: 10,
			},
		]);
		expect(d.moves).toEqual([[SOURCE, "a.m4a"]]);
		expect(d.sizes.has(SOURCE)).toBe(false);
	});

	it("does not move anything when the file is already in the queue directory", async () => {
		const d = disk({ "a.m4a": 80_000 });
		expect(await planRecovery(interrupted, d.api)).toMatchObject([
			{ kind: "queued", byteCount: 80_000 },
		]);
		expect(d.moves).toEqual([]);
	});

	it("fails a row whose recorder file is gone and cannot be adopted", async () => {
		const d = disk({});
		expect(await planRecovery(interrupted, d.api)).toEqual([
			{
				recordingID: "a",
				kind: "failed",
				lastError: "Recording was interrupted before it was saved",
			},
		]);
	});

	it("fails a row without a source when the queued file is missing", async () => {
		const noSource = addRecording(
			EMPTY_INDEX,
			{ ...base, recordingID: "a" },
			"recording",
		);
		const d = disk({ [SOURCE]: 80_000 });
		expect(await planRecovery(noSource, d.api)).toMatchObject([
			{ kind: "failed" },
		]);
		expect(d.moves).toEqual([]);
	});

	it("queues a recording whose file survived, estimating the duration", async () => {
		expect(await planRecovery(interrupted, files())).toEqual([
			{
				recordingID: "a",
				kind: "queued",
				byteCount: 80_000,
				sha256: "HASH",
				durationSeconds: 10,
			},
		]);
	});

	it("keeps a known duration", async () => {
		const index = addRecording(
			EMPTY_INDEX,
			{ ...base, recordingID: "a", durationSeconds: 42 },
			"recording",
		);
		const [patch] = await planRecovery(index, files());
		expect(patch).toMatchObject({ kind: "queued", durationSeconds: 42 });
	});

	it("fails a recording with a missing or empty file or an unreadable size", async () => {
		for (const broken of [
			files({ size: () => 0 }),
			files({
				size: () => {
					throw new Error("stat");
				},
			}),
			files({
				size: () => 0,
				adopt: async () => {
					throw new Error("missing");
				},
			}),
		]) {
			expect(await planRecovery(interrupted, broken)).toEqual([
				{
					recordingID: "a",
					kind: "failed",
					lastError: "Recording was interrupted before it was saved",
				},
			]);
		}
	});

	it("fails a recording whose hash cannot be computed", async () => {
		expect(
			await planRecovery(
				interrupted,
				files({ sha256: async () => Promise.reject(new Error("io")) }),
			),
		).toEqual([{ recordingID: "a", kind: "failed", lastError: "io" }]);
	});

	it("ignores every other state", async () => {
		const queued = addRecording(EMPTY_INDEX, {
			...base,
			recordingID: "q",
			sha256: "X",
		});
		expect(await planRecovery(queued, files())).toEqual([]);
	});
});

describe("applyRecovery", () => {
	it("patches and queues, or fails, the rows named", () => {
		const next = applyRecovery(interrupted, [
			{
				recordingID: "a",
				kind: "queued",
				byteCount: 5,
				sha256: "H",
				durationSeconds: 1,
			},
		]);
		expect(next.recordings[0]).toMatchObject({
			state: "queued",
			byteCount: 5,
			sha256: "H",
			durationSeconds: 1,
		});
		const failed = applyRecovery(interrupted, [
			{ recordingID: "a", kind: "failed", lastError: "gone" },
		]);
		expect(failed.recordings[0]).toMatchObject({
			state: "failed",
			lastError: "gone",
		});
	});

	it("leaves rows alone that moved on or vanished since the plan", () => {
		const moved = setState(interrupted, "a", "queued");
		const patches = [
			{ recordingID: "a", kind: "failed" as const, lastError: "late" },
			{ recordingID: "zz", kind: "failed" as const, lastError: "late" },
		];
		expect(applyRecovery(moved, patches)).toBe(moved);
		expect(applyRecovery(interrupted, [])).toBe(interrupted);
	});
});
