import { stenoLink } from "@modules/steno-link/native";
import {
	type RecordingStatus,
	requestRecordingPermissionsAsync,
	setAudioModeAsync,
	useAudioRecorder,
} from "expo-audio";
import { randomUUID } from "expo-crypto";
import { File } from "expo-file-system";
import { useCallback, useEffect, useRef, useState } from "react";
import { queuedFile } from "@/features/queue/queue-files";
import { errorMessage } from "@/lib/error-message";
import {
	RECORDING_AUDIO_MODE,
	RECORDING_OPTIONS,
	recordingFileName,
} from "./recording-options";

/**
 * expo-audio wrapper (plan P3). One recording at a time: request the
 * microphone, take exclusive audio focus, record to expo-audio's directory
 * under Documents, and on stop move the file into
 * `Documents/queue/<recordingID>.m4a` and hash it natively. Enqueueing is the
 * caller's job through the callbacks so this hook stays free of queue state.
 *
 * Interruptions (a call, Siri) pause the recorder without any status event;
 * iOS resumes it only when the interruption ends with `shouldResume`. The
 * hook polls once a second while a session is active, reports `paused` when
 * the recorder stopped taking audio, and offers `resume()`.
 */
export type RecordingSession = {
	recordingID: string;
	startedAt: Date;
	/** Where expo-audio writes; crash recovery moves the file from here. */
	sourceUri: string | null;
};

export type FinishedRecording = {
	recordingID: string;
	fileName: string;
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

/** The poll that keeps `elapsedSeconds` current and notices a paused recorder. */
export const RECORDER_POLL_MS = 1000;
/** Ticks without `isRecording` before the session counts as paused. */
const PAUSED_AFTER_TICKS = 2;

/** Moves the finished file into the queue directory and hashes it. */
async function finalizeRecording(
	uri: string,
	session: RecordingSession,
	durationSeconds: number,
): Promise<FinishedRecording> {
	const fileName = recordingFileName(session.recordingID);
	const destination = queuedFile(fileName);
	const source = new File(uri);
	if (source.uri !== destination.uri) {
		await source.move(destination, { overwrite: true });
	}
	const byteCount = destination.size;
	if (!byteCount) {
		throw new Error("Recording file is empty");
	}
	const sha256 = await stenoLink().sha256(destination.uri);
	return {
		recordingID: session.recordingID,
		fileName,
		startedAt: session.startedAt.toISOString(),
		durationSeconds,
		byteCount,
		sha256,
	};
}

export type RecorderHandle = {
	isRecording: boolean;
	/** The recorder stopped taking audio (an interruption) while a session is active. */
	paused: boolean;
	session: RecordingSession | null;
	/** Elapsed seconds of the active recording, refreshed once a second; 0 when idle. */
	elapsedSeconds: number;
	start(): Promise<void>;
	stop(): Promise<void>;
	/** Restarts a paused recorder; a no-op otherwise. */
	resume(): void;
};

export function useRecorder(callbacks: RecorderCallbacks): RecorderHandle {
	const [session, setSession] = useState<RecordingSession | null>(null);
	const [elapsedSeconds, setElapsedSeconds] = useState(0);
	const [paused, setPaused] = useState(false);
	const sessionRef = useRef<RecordingSession | null>(null);
	const stoppingRef = useRef(false);
	const callbacksRef = useRef(callbacks);
	callbacksRef.current = callbacks;
	/** Last polled elapsed value; the duration when an interruption ends the recording. */
	const lastKnownSeconds = useRef(0);

	const endSession = useCallback(() => {
		sessionRef.current = null;
		setSession(null);
		setPaused(false);
		setElapsedSeconds(0);
	}, []);

	const finish = useCallback(
		async (uri: string | null, durationSeconds: number) => {
			const current = sessionRef.current;
			if (!current) return;
			endSession();
			if (!uri) {
				callbacksRef.current.onFailed(current, "Recording produced no file");
				return;
			}
			try {
				const finished = await finalizeRecording(uri, current, durationSeconds);
				callbacksRef.current.onFinished(finished);
			} catch (error) {
				callbacksRef.current.onFailed(current, errorMessage(error));
			}
		},
		[endSession],
	);

	const recorder = useAudioRecorder(
		RECORDING_OPTIONS,
		(status: RecordingStatus) => {
			// `isFinished` without our `stop()` means the recorder ended on its
			// own (expo-audio reports it for a finished `recordForDuration` or a
			// stop from outside the hook). The file on disk is valid up to that
			// point, so queue it. Interruptions do not arrive here; the poll
			// below notices them.
			if (status.isFinished && sessionRef.current && !stoppingRef.current) {
				void finish(status.url, lastKnownSeconds.current);
			} else if (
				status.hasError &&
				sessionRef.current &&
				!stoppingRef.current
			) {
				const current = sessionRef.current;
				endSession();
				callbacksRef.current.onFailed(
					current,
					status.error ?? "Recording error",
				);
			}
		},
	);

	// While a session is active: refresh the elapsed time and notice a
	// recorder that stopped taking audio without telling us.
	useEffect(() => {
		if (!session) return;
		let idleTicks = 0;
		const timer = setInterval(() => {
			if (!sessionRef.current || stoppingRef.current) return;
			lastKnownSeconds.current = recorder.currentTime;
			setElapsedSeconds(recorder.currentTime);
			idleTicks = recorder.isRecording ? 0 : idleTicks + 1;
			setPaused(idleTicks >= PAUSED_AFTER_TICKS);
		}, RECORDER_POLL_MS);
		return () => clearInterval(timer);
	}, [session, recorder]);

	const start = useCallback(async () => {
		if (sessionRef.current) return;
		const permission = await requestRecordingPermissionsAsync();
		if (!permission.granted) {
			throw new Error("Microphone permission was not granted");
		}
		await setAudioModeAsync(RECORDING_AUDIO_MODE);
		await recorder.prepareToRecordAsync();
		const next: RecordingSession = {
			recordingID: randomUUID(),
			startedAt: new Date(),
			sourceUri: recorder.uri,
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

	const resume = useCallback(() => {
		if (!sessionRef.current || stoppingRef.current || recorder.isRecording) {
			return;
		}
		// expo-audio accepts `record()` from `paused`; the poll clears `paused`
		// once `isRecording` is true again.
		recorder.record();
	}, [recorder]);

	return {
		isRecording: session !== null,
		paused,
		session,
		elapsedSeconds,
		start,
		stop,
		resume,
	};
}
