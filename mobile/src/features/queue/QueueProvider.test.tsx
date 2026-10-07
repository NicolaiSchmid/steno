// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

/**
 * The provider over a queue directory whose index read fails while
 * `fake.readFails` is set (a launch before the first unlock), and a fake
 * `AppState` emitter.
 */
const fake = vi.hoisted(() => {
	const listeners = new Set<(state: string) => void>();
	return {
		readFails: true,
		reads: 0,
		listeners,
		emit(state: string) {
			for (const listener of listeners) listener(state);
		},
	};
});

vi.mock("react-native", () => ({
	AppState: {
		addEventListener: (_: string, listener: (state: string) => void) => {
			fake.listeners.add(listener);
			return { remove: () => fake.listeners.delete(listener) };
		},
	},
}));
vi.mock("./queue-files", () => ({
	ensureQueueDirectory: () => ({ uri: "file:///docs/queue" }),
	recorderDirectory: () => ({ uri: "file:///docs/ExpoAudio" }),
	expoQueueFiles: {
		async readText() {
			fake.reads += 1;
			if (fake.readFails) throw new Error("EACCES read");
			return null;
		},
		async writeText() {},
		async rename() {},
		async move() {},
		async list() {
			return [];
		},
	},
}));

import {
	type QueueContextValue,
	QueueProvider,
	useQueue,
} from "./QueueProvider";

(
	globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;

afterEach(() => {
	act(() => root?.unmount());
	root = null;
});

async function mount() {
	let value!: QueueContextValue;
	function Probe() {
		value = useQueue();
		return null;
	}
	root = createRoot(document.createElement("div"));
	await act(async () =>
		root?.render(
			<QueueProvider>
				<Probe />
			</QueueProvider>,
		),
	);
	return {
		get value() {
			return value;
		},
	};
}

describe("QueueProvider", () => {
	it("loads again when the app comes to the foreground, until a load succeeds", async () => {
		const queue = await mount();
		expect(queue.value).toMatchObject({
			ready: false,
			loadError: "EACCES read",
		});

		fake.readFails = false;
		await act(async () => fake.emit("background"));
		expect(queue.value.ready).toBe(false);
		await act(async () => fake.emit("active"));
		expect(queue.value).toMatchObject({ ready: true, loadError: null });

		const reads = fake.reads;
		await act(async () => fake.emit("active"));
		expect(fake.reads).toBe(reads);
	});
});
