import type { UploadFailed, UploadFinished } from "@modules/steno-link";

import { HandoverError } from "@/features/pairing/pairing-client";
import {
	chunkPlan,
	findRecording,
	isPending,
	markChunk,
	type QueueIndex,
	scheduleRetry,
	setState,
	syncChunks,
	unpairPending,
} from "@/features/queue/queue-index";
import { errorMessage } from "@/lib/error-message";
import type {
	announce,
	complete,
	Session,
	startChunkUpload,
	status,
} from "./recording-client";
import { metadataFor } from "./recording-client";
import {
	type Action,
	backoffMs,
	parseChunkTaskID,
	taskIDs,
} from "./upload-coordinator";

/**
 * Executes one planner action and feeds the Mac's answers back into the
 * queue (plan P6). Every effect goes through the injected dependencies so
 * the 401 / 404 / 409 / 422 paths, the retry backoff and the resume after
 * partial chunks are unit-tested against a scripted fake of the Mac;
 * `use-upload-coordinator.ts` wires the real module, files and clock.
 */
export type RecordingClient = {
	announce: typeof announce;
	status: typeof status;
	complete: typeof complete;
	startChunkUpload: typeof startChunkUpload;
};

/** The recording files in `Documents/queue/`, by file name. */
export type RecordingFiles = {
	exists(fileName: string): boolean;
	uri(fileName: string): string;
	remove(fileName: string): void;
};

export type ExecutorDependencies = {
	client: RecordingClient;
	files: RecordingFiles;
	deviceName(): Promise<string>;
	update(transform: (index: QueueIndex) => QueueIndex): Promise<unknown>;
	/** Called after every pending row became `unpaired`; forgets the pairing. */
	onUnauthorized(): Promise<void>;
	now(): Date;
	random(): number;
};

export type UploadExecutor = {
	/** Task ids in flight: announces, chunks in the background session, completes. */
	readonly inFlight: Set<string>;
	execute(action: Action, session: Session, index: QueueIndex): Promise<void>;
	/** A transient failure or a 401 for one recording. */
	fail(recordingID: string, error: unknown): Promise<void>;
	uploadFinished(event: UploadFinished): Promise<void>;
	uploadFailed(event: UploadFailed): Promise<void>;
	/**
	 * Re-reads the Mac's chunk set for every `uploading` row, so chunks that
	 * finished while JS was dead count; a 404 sends the row back to `queued`.
	 */
	refreshUploading(
		session: Session,
		index: QueueIndex,
		cancelled?: () => boolean,
	): Promise<void>;
};

export function createUploadExecutor(
	deps: ExecutorDependencies,
): UploadExecutor {
	const inFlight = new Set<string>();

	const unauthorized = async () => {
		await deps.update(unpairPending);
		await deps.onUnauthorized();
	};

	const fail = async (recordingID: string, error: unknown) => {
		if (error instanceof HandoverError && error.kind === "unauthorized") {
			await unauthorized();
			return;
		}
		await deps.update((current) => {
			const rec = findRecording(current, recordingID);
			if (!rec || !isPending(rec)) return current;
			return scheduleRetry(
				current,
				recordingID,
				deps.now(),
				backoffMs(rec.attempts + 1, deps.random),
				errorMessage(error),
			);
		});
	};

	const execute = async (
		action: Action,
		session: Session,
		index: QueueIndex,
	) => {
		switch (action.kind) {
			case "announce": {
				const rec = findRecording(index, action.recordingID);
				if (!rec) return;
				const id = taskIDs.announce(rec.recordingID);
				inFlight.add(id);
				try {
					if (!deps.files.exists(rec.fileName)) {
						await deps.update((current) =>
							setState(current, rec.recordingID, "failed", {
								lastError: "The recording file is missing",
							}),
						);
						return;
					}
					const deviceName = await deps.deviceName();
					const result = await deps.client.announce(
						session,
						metadataFor(rec, deviceName),
					);
					await deps.update((current) =>
						setState(
							syncChunks(current, rec.recordingID, result.receivedChunks),
							rec.recordingID,
							"uploading",
							{ lastError: null },
						),
					);
				} catch (error) {
					await fail(rec.recordingID, error);
				} finally {
					inFlight.delete(id);
				}
				return;
			}
			case "upload-chunk": {
				const rec = findRecording(index, action.recordingID);
				if (!rec) return;
				const chunk = chunkPlan(rec.byteCount, rec.chunkSize)[action.chunk];
				if (!chunk) return;
				const id = taskIDs.chunk(rec.recordingID, action.chunk);
				inFlight.add(id);
				try {
					await deps.client.startChunkUpload(
						session,
						rec.recordingID,
						chunk,
						deps.files.uri(rec.fileName),
					);
				} catch (error) {
					inFlight.delete(id);
					await fail(rec.recordingID, error);
				}
				return;
			}
			case "complete": {
				const rec = findRecording(index, action.recordingID);
				if (!rec) return;
				const id = taskIDs.complete(rec.recordingID);
				inFlight.add(id);
				try {
					const result = await deps.client.complete(session, rec.recordingID);
					if (result.kind === "complete") {
						await deps.update((current) =>
							setState(current, rec.recordingID, "delivered", {
								meetingID: result.meetingID,
								lastError: null,
							}),
						);
						if (deps.files.exists(rec.fileName)) {
							deps.files.remove(rec.fileName);
						}
					} else if (result.kind === "missing-chunks") {
						const remote = await deps.client.status(session, rec.recordingID);
						await deps.update((current) =>
							syncChunks(current, rec.recordingID, remote.receivedChunks),
						);
						// The Mac has every chunk yet refuses to complete (still
						// verifying, or the two sides disagree on the plan): back
						// off instead of posting `complete` again on the same tick.
						const planned = chunkPlan(rec.byteCount, rec.chunkSize).length;
						if (new Set(remote.receivedChunks).size >= planned) {
							await fail(
								rec.recordingID,
								new HandoverError(
									"server",
									409,
									"The Mac is not ready to complete the upload",
								),
							);
						}
					} else {
						await deps.update((current) =>
							setState(
								syncChunks(current, rec.recordingID, []),
								rec.recordingID,
								"failed",
								{ lastError: "The Mac received a damaged file" },
							),
						);
					}
				} catch (error) {
					await fail(rec.recordingID, error);
				} finally {
					inFlight.delete(id);
				}
				return;
			}
			case "wait":
			case "idle":
				return;
		}
	};

	const uploadFinished = async ({ taskID, status }: UploadFinished) => {
		inFlight.delete(taskID);
		const parsed = parseChunkTaskID(taskID);
		if (!parsed) return;
		const { recordingID, chunk } = parsed;
		if (status === 204 || status === 200) {
			await deps.update((current) =>
				findRecording(current, recordingID)
					? markChunk(current, recordingID, chunk)
					: current,
			);
		} else if (status === 401) {
			await unauthorized();
		} else if (status === 404) {
			// The Mac lost the partial (restart, cleanup): announce again after
			// the backoff, so a Mac that keeps forgetting does not cost a
			// 16 MiB copy per tick.
			await deps.update((current) => {
				const rec = findRecording(current, recordingID);
				if (rec?.state !== "uploading") return current;
				return scheduleRetry(
					syncChunks(current, recordingID, []),
					recordingID,
					deps.now(),
					backoffMs(rec.attempts + 1, deps.random),
					"The Mac forgot the upload; starting over",
				);
			});
		} else {
			await fail(
				recordingID,
				new HandoverError(
					"server",
					status,
					`Chunk ${chunk} was answered ${status}`,
				),
			);
		}
	};

	// Every failure backs off, cancelled or not: a rejected pin arrives as a
	// cancellation too, and re-planning it at once would loop through a
	// 16 MiB copy and a TLS handshake per tick.
	const uploadFailed = async ({ taskID, message }: UploadFailed) => {
		inFlight.delete(taskID);
		const parsed = parseChunkTaskID(taskID);
		if (!parsed) return;
		await fail(parsed.recordingID, new Error(message));
	};

	const refreshUploading = async (
		session: Session,
		index: QueueIndex,
		cancelled: () => boolean = () => false,
	) => {
		for (const rec of index.recordings) {
			if (rec.state !== "uploading" || cancelled()) continue;
			try {
				const remote = await deps.client.status(session, rec.recordingID);
				await deps.update((current) =>
					findRecording(current, rec.recordingID)?.state === "uploading"
						? syncChunks(current, rec.recordingID, remote.receivedChunks)
						: current,
				);
			} catch (error) {
				if (error instanceof HandoverError && error.kind === "not-found") {
					await deps.update((current) =>
						findRecording(current, rec.recordingID)?.state === "uploading"
							? setState(
									syncChunks(current, rec.recordingID, []),
									rec.recordingID,
									"queued",
								)
							: current,
					);
				} else if (
					error instanceof HandoverError &&
					error.kind === "unauthorized"
				) {
					await unauthorized();
					return;
				}
			}
		}
	};

	return {
		inFlight,
		execute,
		fail,
		uploadFinished,
		uploadFailed,
		refreshUploading,
	};
}
