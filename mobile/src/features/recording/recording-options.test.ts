import { describe, expect, it } from "vitest";

import {
	CHUNK_SIZE,
	RECORDING_AUDIO_MODE,
	RECORDING_FORMAT,
	RECORDING_OPTIONS,
	recordingFileName,
} from "./recording-options";

describe("recording preset", () => {
	it("is mono AAC m4a at 44.1 kHz and 64 kbps in Documents", () => {
		expect(RECORDING_FORMAT).toBe("m4aAAC");
		expect(RECORDING_OPTIONS).toMatchObject({
			directory: "document",
			extension: ".m4a",
			sampleRate: 44_100,
			numberOfChannels: 1,
			bitRate: 64_000,
			ios: { outputFormat: "aac " },
		});
	});

	it("takes exclusive audio focus and records in silent mode and background", () => {
		expect(RECORDING_AUDIO_MODE).toEqual({
			allowsRecording: true,
			allowsBackgroundRecording: true,
			playsInSilentMode: true,
			interruptionMode: "doNotMix",
			shouldPlayInBackground: true,
		});
	});

	it("chunks at 16 MiB and names files by recording id", () => {
		expect(CHUNK_SIZE).toBe(16_777_216);
		expect(recordingFileName("abc")).toBe("abc.m4a");
	});
});
