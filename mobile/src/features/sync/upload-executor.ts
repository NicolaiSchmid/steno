import type { UploadFailed, UploadFinished } from "@modules/steno-link";

import { HandoverError } from "@modules/steno-link/native";

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
	MacSession,
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
	/**
	 * A 401 answered a request sent with `token` (`null` when nobody
	 * recorded it, which stands for the pairing read from the keychain).
	 * While `token` is still the pairing's, the Mac revoked it: runs
	 * `unpairRows` (every pending row becomes `unpaired`), forgets the
	 * pairing and resolves true.
	 * Resolves false for a pairing since replaced or cleared; the executor
	 * then handles the 401 like any other failure of that request, except
	 * that a refresh of the chunk sets stops there.
	 */
	onUnauthorized(
		token: string | null,
		unpairRows: () => Promise<unknown>,
	): Promise<boolean>;
	now(): Date;
	random(): number;
};

export type UploadExecutor = {
	/** Task ids in flight: announces, chunks in the background session, completes. */
	readonly inFlight: Set<string>;
	execute(
		action: Action,
		session: MacSession,
		index: QueueIndex,
	): Promise<void>;
	/** A transient failure or a 401 for one recording. */
	fail(recordingID: string, error: unknown): Promise<void>;
	uploadFinished(event: UploadFinished): Promise<void>;
	uploadFailed(event: UploadFailed): Promise<void>;
	/**
	 * Replaces the chunk ids in flight with what the background session
	 * reports (`pendingUploads()`), so a dropped `uploadFinished` never parks
	 * a recording; announce and complete ids are untouched.
	 */
	reconcile(pendingTaskIDs: readonly string[]): void;
	/**
	 * Re-reads the Mac's chunk set for every `uploading` row, so chunks that
	 * finished while JS was dead count; a 404 sends the row back to `queued`.
	 */
	refreshUploading(
		session: MacSession,
		index: QueueIndex,
		cancelled?: () => boolean,
	): Promise<void>;
};

export function createUploadExecutor(
	deps: ExecutorDependencies,
): UploadExecutor {
	const inFlight = new Set<string>();
	// The token each chunk in the background session was started with, so
	// its 401 is judged against the pairing it was sent under. A chunk
	// started before a relaunch has none.
	const chunkTokens = new Map<string, string>();

	// Whether the 401 to `token` unpaired the phone.
	const unauthorized = (token: string | null) =>
		deps.onUnauthorized(token, () => deps.update(unpairPending));

	const fail = async (
		recordingID: string,
		error: unknown,
		token: string | null = null,
	) => {
		let message = errorMessage(error);
		if (error instanceof HandoverError && error.kind === "unauthorized") {
			if (await unauthorized(token)) return;
			// A 401 to a pairing since replaced: not a revoke of this one.
			message = "Retrying";
		}
		await deps.update((current) => {
			const rec = findRecording(current, recordingID);
			if (!rec || !isPending(rec)) return current;
			return scheduleRetry(
				current,
				recordingID,
				deps.now(),
				backoffMs(rec.attempts + 1, deps.random),
				message,
			);
		});
	};

	const execute = async (
		action: Action,
		session: MacSession,
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
					await fail(rec.recordingID, error, session.token);
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
				chunkTokens.set(id, session.token);
				try {
					await deps.client.startChunkUpload(
						session,
						rec.recordingID,
						chunk,
						deps.files.uri(rec.fileName),
					);
				} catch (error) {
					inFlight.delete(id);
					chunkTokens.delete(id);
					await fail(rec.recordingID, error, session.token);
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
						// The row is delivered also when an unpair moved it to
						// `unpaired`, or a new pairing queued it again, while the
						// request was out: the Mac admitted the meeting, so the phone
						// keeps no copy to upload again. The planner announces nothing
						// for the row until this answer lands, so no new upload reads
						// the file removed here.
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
					await fail(rec.recordingID, error, session.token);
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
		const token = chunkTokens.get(taskID) ?? null;
		chunkTokens.delete(taskID);
		const parsed = parseChunkTaskID(taskID);
		if (!parsed) return;
		const { recordingID, chunk } = parsed;
		if (status === 204 || status === 200) {
			await deps.update((current) =>
				findRecording(current, recordingID)
					? markChunk(current, recordingID, chunk)
					: current,
			);
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
					status === 401 ? "unauthorized" : "server",
					status,
					`Chunk ${chunk} was answered ${status}`,
				),
				token,
			);
		}
	};

	// Every failure backs off, cancelled or not: a rejected pin arrives as a
	// cancellation too, and re-planning it at once would loop through a
	// 16 MiB copy and a TLS handshake per tick.
	const uploadFailed = async ({ taskID, message }: UploadFailed) => {
		inFlight.delete(taskID);
		chunkTokens.delete(taskID);
		const parsed = parseChunkTaskID(taskID);
		if (!parsed) return;
		await fail(parsed.recordingID, new Error(message));
	};

	const reconcile = (pendingTaskIDs: readonly string[]) => {
		for (const id of inFlight) {
			if (parseChunkTaskID(id)) inFlight.delete(id);
		}
		for (const id of pendingTaskIDs) inFlight.add(id);
		for (const id of chunkTokens.keys()) {
			if (!inFlight.has(id)) chunkTokens.delete(id);
		}
	};

	const refreshUploading = async (
		session: MacSession,
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
					// Revoked, or a pairing since replaced whose session is about
					// to go: either way the other rows would only answer 401 too.
					await unauthorized(session.token);
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
		reconcile,
		refreshUploading,
	};
}
