// @vitest-environment happy-dom
import type { RecordingStatus } from "expo-audio";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
	type RecorderCallbacks,
	type RecorderHandle,
	useRecorder,
} from "./recorder";
import { RECORDING_AUDIO_MODE, RECORDING_OPTIONS } from "./recording-options";

/**
 * Fakes for the native surface: expo-audio's recorder and status listener,
 * expo-file-system's `File` over an in-memory table of sizes, expo-crypto's
 * ids, the queue directory and the module's hash. No timers, no disk.
 */
const fake = vi.hoisted(() => {
	const files = new Map<string, number>();
	const moves: [string, string][] = [];
	class File {
		readonly uri: string;
		constructor(...parts: (string | { uri: string })[]) {
			// Join like expo-file-system, keeping the `file://` scheme's slashes.
			this.uri = parts
				.map((p) => (typeof p === "string" ? p : p.uri))
				.join("/")
				.replace(/([^:/])\/{2,}/g, "$1/");
		}
		get exists() {
			return files.has(this.uri);
		}
		get size() {
			return files.get(this.uri) ?? 0;
		}
		async move(to: File) {
			const size = files.get(this.uri);
			if (size === undefined) throw new Error(`missing ${this.uri}`);
			files.delete(this.uri);
			files.set(to.uri, size);
			moves.push([this.uri, to.uri]);
		}
	}
	const recorder = {
		currentTime: 0,
		uri: null as string | null,
		prepareToRecordAsync: vi.fn(async () => {}),
		record: vi.fn(),
		stop: vi.fn(async () => {}),
	};
	let uuid = 0;
	return {
		files,
		moves,
		File,
		recorder,
		options: null as unknown,
		listener: null as ((status: RecordingStatus) => void) | null,
		permission: { granted: true },
		requestRecordingPermissionsAsync: vi.fn(async () => fake.permission),
		setAudioModeAsync: vi.fn(async () => {}),
		sha256: vi.fn(async (uri: string) => `sha(${uri})`),
		nextUUID: () => `rec-${++uuid}`,
		reset() {
			files.clear();
			moves.length = 0;
			recorder.currentTime = 0;
			recorder.uri = null;
			recorder.prepareToRecordAsync.mockClear();
			recorder.record.mockClear();
			recorder.stop.mockClear();
			recorder.stop.mockImplementation(async () => {});
			this.permission = { granted: true };
			this.requestRecordingPermissionsAsync.mockClear();
			this.setAudioModeAsync.mockClear();
			this.sha256.mockClear();
			uuid = 0;
		},
	};
});

vi.mock("expo-audio", () => ({
	useAudioRecorder: (
		options: unknown,
		listener: (status: RecordingStatus) => void,
	) => {
		fake.options = options;
		fake.listener = listener;
		return fake.recorder;
	},
	requestRecordingPermissionsAsync: fake.requestRecordingPermissionsAsync,
	setAudioModeAsync: fake.setAudioModeAsync,
}));
vi.mock("expo-crypto", () => ({ randomUUID: () => fake.nextUUID() }));
vi.mock("expo-file-system", () => ({ File: fake.File }));
vi.mock("@modules/steno-link", () => ({
	stenoLink: () => ({ sha256: fake.sha256 }),
}));
vi.mock("@/features/queue/queue-files", () => ({
	ensureQueueDirectory: () => ({ uri: "file:///docs/queue" }),
}));

(
	globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

const SOURCE = "file:///docs/recording-123.m4a";

function status(partial: Partial<RecordingStatus>): RecordingStatus {
	return {
		id: "1",
		isFinished: false,
		hasError: false,
		error: null,
		url: null,
		...partial,
	};
}

let root: Root | null = null;

function mount() {
	const callbacks: RecorderCallbacks = {
		onStarted: vi.fn(),
		onFinished: vi.fn(),
		onFailed: vi.fn(),
	};
	let handle!: RecorderHandle;
	function Harness() {
		handle = useRecorder(callbacks);
		return null;
	}
	root = createRoot(document.createElement("div"));
	act(() => root?.render(<Harness />));
	return {
		callbacks,
		get handle() {
			return handle;
		},
		start: () => act(() => handle.start()),
		stop: () => act(() => handle.stop()),
		emit: (s: RecordingStatus) => act(async () => fake.listener?.(s)),
	};
}

async function startWithFile(size = 4096) {
	const h = mount();
	await h.start();
	fake.recorder.uri = SOURCE;
	fake.files.set(SOURCE, size);
	return h;
}

beforeEach(() => fake.reset());
afterEach(() => {
	act(() => root?.unmount());
	root = null;
});

describe("start", () => {
	it("asks for the microphone, takes exclusive audio focus, prepares with the preset and records", async () => {
		const h = mount();
		expect(h.handle.isRecording).toBe(false);
		expect(fake.options).toBe(RECORDING_OPTIONS);

		await h.start();

		expect(fake.requestRecordingPermissionsAsync).toHaveBeenCalledTimes(1);
		expect(fake.setAudioModeAsync).toHaveBeenCalledWith(RECORDING_AUDIO_MODE);
		expect(fake.recorder.prepareToRecordAsync).toHaveBeenCalledTimes(1);
		expect(fake.recorder.record).toHaveBeenCalledTimes(1);
		expect(h.handle.isRecording).toBe(true);
		expect(h.callbacks.onStarted).toHaveBeenCalledWith({
			recordingID: "rec-1",
			startedAt: expect.any(Date),
		});
		expect(h.handle.session?.recordingID).toBe("rec-1");
	});

	it("throws and stays idle when the permission is denied", async () => {
		fake.permission = { granted: false };
		const h = mount();
		await expect(h.start()).rejects.toThrow(/permission/);
		expect(fake.setAudioModeAsync).not.toHaveBeenCalled();
		expect(fake.recorder.record).not.toHaveBeenCalled();
		expect(h.handle.isRecording).toBe(false);
		expect(h.callbacks.onStarted).not.toHaveBeenCalled();
	});

	it("is a no-op while a recording is running", async () => {
		const h = mount();
		await h.start();
		await h.start();
		expect(fake.recorder.prepareToRecordAsync).toHaveBeenCalledTimes(1);
		expect(h.callbacks.onStarted).toHaveBeenCalledTimes(1);
	});
});

describe("stop", () => {
	it("stops, moves the file into the queue directory under the recording id and hashes it", async () => {
		const h = await startWithFile(4096);
		fake.recorder.currentTime = 12.5;

		await h.stop();

		expect(fake.recorder.stop).toHaveBeenCalledTimes(1);
		expect(fake.moves).toEqual([[SOURCE, "file:///docs/queue/rec-1.m4a"]]);
		expect(fake.sha256).toHaveBeenCalledWith("file:///docs/queue/rec-1.m4a");
		expect(h.callbacks.onFinished).toHaveBeenCalledWith({
			recordingID: "rec-1",
			fileName: "rec-1.m4a",
			startedAt: expect.stringMatching(/^\d{4}-.*Z$/),
			durationSeconds: 12.5,
			byteCount: 4096,
			sha256: "sha(file:///docs/queue/rec-1.m4a)",
		});
		expect(h.callbacks.onFailed).not.toHaveBeenCalled();
		expect(h.handle.isRecording).toBe(false);
		expect(h.handle.elapsedSeconds()).toBe(0);
	});

	it("does not move a file that is already in place", async () => {
		const h = mount();
		await h.start();
		fake.recorder.uri = "file:///docs/queue/rec-1.m4a";
		fake.files.set("file:///docs/queue/rec-1.m4a", 10);
		await h.stop();
		expect(fake.moves).toEqual([]);
		expect(h.callbacks.onFinished).toHaveBeenCalledTimes(1);
	});

	it("reports an empty file as a failure and never hashes it", async () => {
		const h = await startWithFile(0);
		await h.stop();
		expect(fake.sha256).not.toHaveBeenCalled();
		expect(h.callbacks.onFinished).not.toHaveBeenCalled();
		expect(h.callbacks.onFailed).toHaveBeenCalledWith(
			expect.objectContaining({ recordingID: "rec-1" }),
			"Recording file is empty",
		);
		expect(h.handle.isRecording).toBe(false);
	});

	it("reports a recorder without a file as a failure", async () => {
		const h = mount();
		await h.start();
		await h.stop();
		expect(h.callbacks.onFailed).toHaveBeenCalledWith(
			expect.objectContaining({ recordingID: "rec-1" }),
			"Recording produced no file",
		);
	});

	it("reports a failed move or hash as a failure with the message", async () => {
		const h = await startWithFile();
		fake.sha256.mockRejectedValueOnce(new Error("ERR_STENO_HASH"));
		await h.stop();
		expect(h.callbacks.onFailed).toHaveBeenCalledWith(
			expect.anything(),
			"ERR_STENO_HASH",
		);
	});

	it("releases the recorder even when stop() throws, and is a no-op when idle", async () => {
		const h = await startWithFile();
		fake.recorder.stop.mockRejectedValueOnce(new Error("AVAudioRecorder"));
		await expect(h.stop()).rejects.toThrow("AVAudioRecorder");
		// Still recording from the hook's point of view; a second stop works.
		expect(h.handle.isRecording).toBe(true);
		await h.stop();
		expect(h.callbacks.onFinished).toHaveBeenCalledTimes(1);
		await h.stop();
		expect(fake.recorder.stop).toHaveBeenCalledTimes(2);
	});
});

describe("interruptions", () => {
	it("queues the file when a call or Siri finishes the recording without stop()", async () => {
		const h = await startWithFile(2048);
		fake.recorder.currentTime = 30;
		// The screen polls once a second; this is the last value it saw.
		expect(h.handle.elapsedSeconds()).toBe(30);

		await h.emit(status({ isFinished: true, url: SOURCE }));

		expect(fake.recorder.stop).not.toHaveBeenCalled();
		expect(h.callbacks.onFinished).toHaveBeenCalledWith(
			expect.objectContaining({
				recordingID: "rec-1",
				fileName: "rec-1.m4a",
				durationSeconds: 30,
				byteCount: 2048,
			}),
		);
		expect(h.handle.isRecording).toBe(false);
	});

	it("reports a media-services reset or recorder error as a failure", async () => {
		const h = await startWithFile();
		await h.emit(
			status({
				hasError: true,
				error: "media services were reset",
				mediaServicesDidReset: true,
			}),
		);
		expect(h.callbacks.onFailed).toHaveBeenCalledWith(
			expect.objectContaining({ recordingID: "rec-1" }),
			"media services were reset",
		);
		expect(h.callbacks.onFinished).not.toHaveBeenCalled();
		expect(fake.moves).toEqual([]);
		expect(h.handle.isRecording).toBe(false);
	});

	it("falls back to a generic message when the error status has none", async () => {
		const h = await startWithFile();
		await h.emit(status({ hasError: true }));
		expect(h.callbacks.onFailed).toHaveBeenCalledWith(
			expect.anything(),
			"Recording error",
		);
	});

	it("ignores the finished status that stop() itself triggers", async () => {
		const h = await startWithFile();
		fake.recorder.stop.mockImplementationOnce(async () => {
			fake.listener?.(status({ isFinished: true, url: SOURCE }));
		});
		await h.stop();
		expect(h.callbacks.onFinished).toHaveBeenCalledTimes(1);
		expect(fake.moves).toHaveLength(1);
	});

	it("ignores status events while nothing is recording", async () => {
		const h = mount();
		await h.emit(status({ isFinished: true, url: SOURCE }));
		await h.emit(status({ hasError: true, error: "x" }));
		expect(h.callbacks.onFinished).not.toHaveBeenCalled();
		expect(h.callbacks.onFailed).not.toHaveBeenCalled();
	});

	it("can start a new recording after an interruption, with a fresh id", async () => {
		const h = await startWithFile();
		await h.emit(status({ isFinished: true, url: SOURCE }));
		await h.start();
		expect(h.callbacks.onStarted).toHaveBeenLastCalledWith(
			expect.objectContaining({ recordingID: "rec-2" }),
		);
		expect(h.handle.isRecording).toBe(true);
	});
});
