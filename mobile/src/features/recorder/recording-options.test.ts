import { describe, expect, it } from "vitest";

import {
	RECORDING_AUDIO_MODE,
	recorderFileID,
	recordingFileName,
	recordingIDFromFileName,
} from "./recording-options";

const ID = "0f8b6a2e-4c1d-4e9a-9b3f-5d7c2a1e8f40";
const RECORDER_ID = ID.toUpperCase();

describe("recording preset", () => {
	it("takes exclusive audio focus and records in silent mode and background", () => {
		expect(RECORDING_AUDIO_MODE).toEqual({
			allowsRecording: true,
			allowsBackgroundRecording: true,
			playsInSilentMode: true,
			interruptionMode: "doNotMix",
			shouldPlayInBackground: true,
		});
	});

	it("reads the recording id back from a recording file name", () => {
		expect(recordingFileName("abc")).toBe("abc.m4a");
		expect(recordingIDFromFileName(recordingFileName(ID))).toBe(ID);
		expect(recordingIDFromFileName(`${RECORDER_ID}.M4A`)).toBe(RECORDER_ID);
	});

	it.each([
		"index.json",
		"index.json.tmp",
		"index.corrupt.json",
		"index.corrupt-1767225600000.json",
		"index.unreadable-1767225600000.json",
		"abc.m4a",
		`${ID}.m4a.tmp`,
		`${ID}xm4a`,
		`${ID}.caf`,
		`x${ID}.m4a`,
	])("reads no recording id from %s", (name) => {
		expect(recordingIDFromFileName(name)).toBeNull();
	});

	it("reads the UUID back from expo-audio's recording file names", () => {
		// `AudioUtils.createRecordingUrl` in expo-audio's iOS module.
		expect(recorderFileID(`recording-${RECORDER_ID}.m4a`)).toBe(ID);
	});

	it.each([
		`${RECORDER_ID}.m4a`,
		`recording-${RECORDER_ID}.caf`,
		`recording-${RECORDER_ID}.m4a.tmp`,
		"recording-abc.m4a",
		`xrecording-${RECORDER_ID}.m4a`,
	])("reads no recorder UUID from %s", (name) => {
		expect(recorderFileID(name)).toBeNull();
	});
});
