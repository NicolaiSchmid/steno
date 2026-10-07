import type { AudioFormat } from "@modules/steno-link";
import type { AudioMode, RecordingOptions } from "expo-audio";

/**
 * The recording preset (plan decision 8): AAC in `.m4a`, mono, 44.1 kHz,
 * 64 kbps, written straight into Documents so a crash never loses the file to
 * the cache sweeper. One hour is about 29 MB.
 *
 * Type-only import from expo-audio keeps this file testable on Node; the two
 * iOS enum values are spelled out (`IOSOutputFormat.MPEG4AAC === "aac "`,
 * `AudioQuality.HIGH === 96`).
 */
export const RECORDING_FORMAT: AudioFormat = "m4aAAC";
export const RECORDING_EXTENSION = ".m4a";

export const RECORDING_OPTIONS: RecordingOptions = {
	directory: "document",
	isMeteringEnabled: false,
	extension: RECORDING_EXTENSION,
	sampleRate: 44_100,
	numberOfChannels: 1,
	bitRate: 64_000,
	ios: {
		outputFormat: "aac ",
		audioQuality: 96,
	},
	android: { outputFormat: "mpeg4", audioEncoder: "aac" },
	web: { mimeType: "audio/mp4", bitsPerSecond: 64_000 },
};

/**
 * Exclusive audio focus; keeps recording through the ringer switch and lock.
 * `allowsBackgroundRecording` is what keeps the recorder running when the
 * screen locks: without it expo-audio pauses every recorder on
 * `OnAppEntersBackground`, whatever `UIBackgroundModes` says.
 */
export const RECORDING_AUDIO_MODE: Partial<AudioMode> = {
	allowsRecording: true,
	allowsBackgroundRecording: true,
	playsInSilentMode: true,
	interruptionMode: "doNotMix",
	shouldPlayInBackground: true,
};

/**
 * The smallest file the queue adopts as a recording; smaller ones stay where
 * they are. expo-audio writes a header of a few dozen bytes when it prepares
 * a file, before any audio, and a second of audio at 64 kbps is 8,000 bytes:
 * below 1 KiB a file holds less than an eighth of a second.
 */
export const MIN_RECORDING_BYTES = 1024;

/** 16 MiB: one hour is two chunks, so a locked phone wakes twice per meeting. */
export const CHUNK_SIZE = 16 * 1024 * 1024;

export function recordingFileName(recordingID: string): string {
	return `${recordingID}${RECORDING_EXTENSION}`;
}

/** `<prefix><UUID>.m4a`, the UUID captured; any case. */
const fileNamePattern = (prefix: string) =>
	new RegExp(
		`^${prefix}([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})${RECORDING_EXTENSION.replace(".", "\\.")}$`,
		"i",
	);

const RECORDING_FILE_NAME = fileNamePattern("");

/**
 * The recording id a `recordingFileName` was made from, or null for any other
 * name (the index files, a file of another app version). The id is the UUID
 * the recorder drew, which the Mac also requires in the upload path.
 */
export function recordingIDFromFileName(fileName: string): string | null {
	return RECORDING_FILE_NAME.exec(fileName)?.[1] ?? null;
}

/**
 * `Documents/ExpoAudio/`, where expo-audio writes a recording while it runs
 * (`RECORDING_OPTIONS.directory` is `document`); the recorder moves the file
 * into the queue directory when it stops.
 */
export const RECORDER_DIRECTORY = "ExpoAudio";

const RECORDER_FILE_NAME = fileNamePattern("recording-");

/**
 * The UUID in a file name expo-audio gives a recording
 * (`recording-<UUID>.m4a`), lower-cased like the ids the recorder draws, or
 * null for any other name.
 */
export function recorderFileID(fileName: string): string | null {
	return RECORDER_FILE_NAME.exec(fileName)?.[1]?.toLowerCase() ?? null;
}
