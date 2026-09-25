import { stenoLink } from "@modules/steno-link";
import {
	type RecordingStatus,
	requestRecordingPermissionsAsync,
	setAudioModeAsync,
	useAudioRecorder,
} from "expo-audio";
import { randomUUID } from "expo-crypto";
import { File } from "expo-file-system";
import { useCallback, useRef, useState } from "react";
import { ensureQueueDirectory } from "@/features/queue/queue-files";
import {
	RECORDING_AUDIO_MODE,
	RECORDING_OPTIONS,
	recordingFileName,
} from "./recording-options";

/**
 * expo-audio wrapper (plan P3). One recording at a time: request the
 * microphone, take exclusive audio focus, record to Documents, and on stop or
 * interruption move the file into `Documents/queue/<recordingID>.m4a` and
 * hash it natively. Enqueueing is the caller's job through the callbacks so
 * this hook stays free of queue state.
 */
export type RecordingSession = {
	recordingID: string;
	startedAt: Date;
};

export type FinishedRecording = {
	recordingID: string;
	fileName: string;
	fileUri: string;
	startedAt: string;
	durationSeconds: number;
	byteCount: number;
	sha256: string;
};

export type RecorderCallbacks = {
	onStarted(session: RecordingSession): void;
	onFinished(recording: FinishedRecording): void;
	/** Recording ended without a usable file; the row should be marked failed. */
	onFailed(session: RecordingSession, message: string): void;
};

export class RecordingPermissionError extends Error {
	constructor() {
		super("Microphone permission was not granted");
		this.name = "RecordingPermissionError";
	}
}

export async function ensureRecordingPermission(): Promise<void> {
	const response = await requestRecordingPermissionsAsync();
	if (!response.granted) throw new RecordingPermissionError();
}

export async function prepareAudioSession(): Promise<void> {
	await setAudioModeAsync(RECORDING_AUDIO_MODE);
}

/** Moves the finished file into the queue directory and hashes it. */
export async function finalizeRecording(args: {
	uri: string;
	session: RecordingSession;
	durationSeconds: number;
}): Promise<FinishedRecording> {
	const fileName = recordingFileName(args.session.recordingID);
	const destination = new File(ensureQueueDirectory(), fileName);
	const source = new File(args.uri);
	if (source.uri !== destination.uri) {
		await source.move(destination, { overwrite: true });
	}
	const byteCount = destination.size;
	if (!byteCount) {
		throw new Error("Recording file is empty");
	}
	const sha256 = await stenoLink().sha256(destination.uri);
	return {
		recordingID: args.session.recordingID,
		fileName,
		fileUri: destination.uri,
		startedAt: args.session.startedAt.toISOString(),
		durationSeconds: args.durationSeconds,
		byteCount,
		sha256,
	};
}

export type RecorderHandle = {
	isRecording: boolean;
	session: RecordingSession | null;
	start(): Promise<void>;
	stop(): Promise<void>;
	/** Elapsed seconds of the active recording; 0 when idle. */
	elapsedSeconds(): number;
};

export function useRecorder(callbacks: RecorderCallbacks): RecorderHandle {
	const [session, setSession] = useState<RecordingSession | null>(null);
	const sessionRef = useRef<RecordingSession | null>(null);
	const stoppingRef = useRef(false);
	const callbacksRef = useRef(callbacks);
	callbacksRef.current = callbacks;
	/** Last elapsed value the UI polled; the duration when an interruption ends the recording. */
	const lastKnownSeconds = useRef(0);

	const finish = useCallback(
		async (uri: string | null, durationSeconds: number) => {
			const current = sessionRef.current;
			if (!current) return;
			sessionRef.current = null;
			setSession(null);
			if (!uri) {
				callbacksRef.current.onFailed(current, "Recording produced no file");
				return;
			}
			try {
				const finished = await finalizeRecording({
					uri,
					session: current,
					durationSeconds,
				});
				callbacksRef.current.onFinished(finished);
			} catch (error) {
				callbacksRef.current.onFailed(
					current,
					error instanceof Error ? error.message : String(error),
				);
			}
		},
		[],
	);

	const recorder = useAudioRecorder(
		RECORDING_OPTIONS,
		(status: RecordingStatus) => {
			// An interruption (call, Siri) or a media-services reset ends the
			// recording without `stop()`. The file on disk is still valid up to
			// that point, so queue it.
			if (status.isFinished && sessionRef.current && !stoppingRef.current) {
				void finish(status.url, lastKnownSeconds.current);
			} else if (
				status.hasError &&
				sessionRef.current &&
				!stoppingRef.current
			) {
				const current = sessionRef.current;
				sessionRef.current = null;
				setSession(null);
				callbacksRef.current.onFailed(
					current,
					status.error ?? "Recording error",
				);
			}
		},
	);

	const start = useCallback(async () => {
		if (sessionRef.current) return;
		await ensureRecordingPermission();
		await prepareAudioSession();
		await recorder.prepareToRecordAsync();
		const next: RecordingSession = {
			recordingID: randomUUID(),
			startedAt: new Date(),
		};
		sessionRef.current = next;
		stoppingRef.current = false;
		lastKnownSeconds.current = 0;
		recorder.record();
		setSession(next);
		callbacksRef.current.onStarted(next);
	}, [recorder]);

	const stop = useCallback(async () => {
		if (!sessionRef.current || stoppingRef.current) return;
		stoppingRef.current = true;
		const durationSeconds = recorder.currentTime;
		try {
			await recorder.stop();
		} finally {
			stoppingRef.current = false;
		}
		await finish(recorder.uri, durationSeconds);
	}, [recorder, finish]);

	const elapsedSeconds = useCallback(() => {
		if (!sessionRef.current) return 0;
		lastKnownSeconds.current = recorder.currentTime;
		return recorder.currentTime;
	}, [recorder]);

	return {
		isRecording: session !== null,
		session,
		start,
		stop,
		elapsedSeconds,
	};
}
