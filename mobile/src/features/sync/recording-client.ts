import {
	authorizationHeader,
	decodeCompleteResponse,
	decodeRecordingStatus,
	paths,
	type RecordingMetadata,
	type RecordingStatus,
	stenoLink,
} from "@modules/steno-link";
import {
	failureFor,
	HandoverError,
	type MacEndpoint,
	pinnedRequest,
} from "@/features/pairing/pairing-client";
import type { Chunk, QueuedRecording } from "@/features/queue/queue-index";
import { RECORDING_FORMAT } from "@/features/recording/recording-options";
import { taskIDs } from "./upload-coordinator";

/**
 * The recording calls: announce (PUT metadata), status (GET), chunk upload
 * (background session) and complete. Foreground calls go through the pinned
 * ephemeral session; chunks through `steno-link`'s background session.
 */
export type Session = { endpoint: MacEndpoint; token: string };

export function metadataFor(
	rec: QueuedRecording,
	deviceName: string,
): RecordingMetadata {
	if (rec.sha256 === null) {
		throw new Error(`recording ${rec.recordingID} has no hash yet`);
	}
	return {
		recordingID: rec.recordingID,
		startedAt: rec.startedAt,
		durationSeconds: rec.durationSeconds,
		byteCount: rec.byteCount,
		sha256: rec.sha256,
		chunkSize: rec.chunkSize,
		format: RECORDING_FORMAT,
		deviceName,
	};
}

function decodeStatus(body: string, status: number): RecordingStatus {
	const decoded = decodeRecordingStatus(body);
	if (!decoded.ok) throw new HandoverError("protocol", status, decoded.reason);
	return decoded.value;
}

/** `PUT /v1/recordings/{id}`: 201 new or 200 existing, both with the status. */
export async function announce(
	session: Session,
	metadata: RecordingMetadata,
): Promise<RecordingStatus> {
	const response = await pinnedRequest(
		session.endpoint,
		"PUT",
		paths.recording(metadata.recordingID),
		{ headers: authorizationHeader("Bearer", session.token), body: metadata },
	);
	const failure = failureFor(response);
	if (failure) throw failure;
	return decodeStatus(response.body, response.status);
}

/** `GET /v1/recordings/{id}` for resume; 404 surfaces as `not-found`. */
export async function status(
	session: Session,
	recordingID: string,
): Promise<RecordingStatus> {
	const response = await pinnedRequest(
		session.endpoint,
		"GET",
		paths.recording(recordingID),
		{ headers: authorizationHeader("Bearer", session.token) },
	);
	const failure = failureFor(response);
	if (failure) throw failure;
	return decodeStatus(response.body, response.status);
}

export type CompleteResult =
	| { kind: "complete"; meetingID: string }
	| { kind: "missing-chunks" }
	| { kind: "hash-mismatch" };

/** `POST /v1/recordings/{id}/complete`: 200, 409 (missing chunks), 422 (hash). */
export async function complete(
	session: Session,
	recordingID: string,
): Promise<CompleteResult> {
	const response = await pinnedRequest(
		session.endpoint,
		"POST",
		paths.complete(recordingID),
		{ headers: authorizationHeader("Bearer", session.token) },
	);
	if (response.status === 409) return { kind: "missing-chunks" };
	if (response.status === 422) return { kind: "hash-mismatch" };
	const failure = failureFor(response);
	if (failure) throw failure;
	const decoded = decodeCompleteResponse(response.body);
	if (!decoded.ok) {
		throw new HandoverError("protocol", response.status, decoded.reason);
	}
	return { kind: "complete", meetingID: decoded.value.meetingID };
}

/**
 * Cancels every chunk still in the background session. Before a re-pairing:
 * a task started under the old token would otherwise finish with 401 and
 * unpair the fresh Mac.
 */
export async function cancelAllUploads(): Promise<void> {
	const link = stenoLink();
	const pending = await link.pendingUploads();
	await Promise.all(pending.map((taskID) => link.cancelUpload(taskID)));
}

/** Hands one chunk to the background session; completion arrives as an event. */
export async function startChunkUpload(
	session: Session,
	recordingID: string,
	chunk: Chunk,
	fileUri: string,
): Promise<void> {
	await stenoLink().startUpload({
		taskID: taskIDs.chunk(recordingID, chunk.index),
		url: `${session.endpoint.origin}${paths.chunk(recordingID, chunk.index)}`,
		headers: authorizationHeader("Bearer", session.token),
		fingerprint: session.endpoint.fingerprint,
		filePath: fileUri,
		offset: chunk.offset,
		length: chunk.length,
	});
}
