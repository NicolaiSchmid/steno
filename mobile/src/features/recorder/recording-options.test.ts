import { describe, expect, it } from "vitest";

import {
	CHUNK_SIZE,
	RECORDING_AUDIO_MODE,
	RECORDING_FORMAT,
	RECORDING_OPTIONS,
	recordingFileName,
	recordingIDFromFileName,
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

	it("reads the recording id back only from a recording file name", () => {
		const id = "0f8b6a2e-4c1d-4e9a-9b3f-5d7c2a1e8f40";
		expect(recordingIDFromFileName(recordingFileName(id))).toBe(id);
		expect(recordingIDFromFileName(`${id.toUpperCase()}.M4A`)).toBe(
			id.toUpperCase(),
		);
		for (const name of [
			"index.json",
			"index.json.tmp",
			"index.corrupt.json",
			"abc.m4a",
			`${id}.m4a.tmp`,
			`${id}xm4a`,
			`${id}.caf`,
			`x${id}.m4a`,
		]) {
			expect(recordingIDFromFileName(name)).toBeNull();
		}
	});
});
