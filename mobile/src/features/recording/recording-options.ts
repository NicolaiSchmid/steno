import type { AudioFormat } from "@modules/steno-link/src/wire";
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

/** 16 MiB: one hour is two chunks, so a locked phone wakes twice per meeting. */
export const CHUNK_SIZE = 16 * 1024 * 1024;

export function recordingFileName(recordingID: string): string {
	return `${recordingID}${RECORDING_EXTENSION}`;
}
