// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/**
 * The keychain answers are scripted per call, and `AppState` is a fake
 * emitter: a background relaunch on a locked phone reads null first and the
 * provider must read again when the app becomes active.
 */
const fake = vi.hoisted(() => {
	const listeners = new Set<(state: string) => void>();
	return {
		loads: [] as (unknown | null)[],
		load: vi.fn(async () => fake.loads.shift() ?? null),
		save: vi.fn(async () => {}),
		clear: vi.fn(async () => {}),
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
vi.mock("./pairing-store", () => ({
	pairingStore: { load: fake.load, save: fake.save, clear: fake.clear },
}));

import {
	type PairingContextValue,
	PairingProvider,
	usePairing,
} from "./PairingProvider";

(
	globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

const pairing = {
	mac: {
		macID: "mac",
		macName: "Studio",
		fingerprint: "FP",
		pairedAt: "2026-09-25T10:00:00.000Z",
	},
	token: "tok",
};

let root: Root | null = null;

async function mount() {
	let value!: PairingContextValue;
	function Probe() {
		value = usePairing();
		return null;
	}
	root = createRoot(document.createElement("div"));
	await act(async () =>
		root?.render(
			<PairingProvider>
				<Probe />
			</PairingProvider>,
		),
	);
	return {
		get value() {
			return value;
		},
	};
}

beforeEach(() => {
	fake.loads.length = 0;
	fake.load.mockClear();
	fake.save.mockClear();
	fake.clear.mockClear();
});
afterEach(() => {
	act(() => root?.unmount());
	root = null;
	fake.listeners.clear();
});

describe("PairingProvider", () => {
	it("hydrates from the keychain once and reports ready", async () => {
		fake.loads.push(pairing);
		const h = await mount();
		expect(h.value).toMatchObject({ pairing, ready: true });
		expect(fake.load).toHaveBeenCalledTimes(1);
	});

	it("reads again on foreground while nothing is loaded, and stops once something is", async () => {
		fake.loads.push(null, pairing);
		const h = await mount();
		expect(h.value).toMatchObject({ pairing: null, ready: true });

		await act(async () => fake.emit("active"));
		expect(h.value.pairing).toEqual(pairing);
		expect(fake.load).toHaveBeenCalledTimes(2);

		await act(async () => fake.emit("active"));
		expect(fake.load).toHaveBeenCalledTimes(2);
	});

	it("ignores background transitions", async () => {
		const h = await mount();
		await act(async () => fake.emit("background"));
		await act(async () => fake.emit("inactive"));
		expect(fake.load).toHaveBeenCalledTimes(1);
		expect(h.value.pairing).toBeNull();
	});

	it("persists before updating state on replace and clear", async () => {
		const h = await mount();
		await act(() => h.value.replace(pairing));
		expect(fake.save).toHaveBeenCalledWith(pairing);
		expect(h.value.pairing).toEqual(pairing);

		await act(() => h.value.clear());
		expect(fake.clear).toHaveBeenCalledTimes(1);
		expect(h.value.pairing).toBeNull();

		// After a clear, the next foreground reads the keychain again.
		fake.loads.push(pairing);
		await act(async () => fake.emit("active"));
		expect(h.value.pairing).toEqual(pairing);
	});
});
