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

/** Holds the next keychain save until the test settles it. */
function holdSave() {
	const save = Promise.withResolvers<void>();
	fake.save.mockReturnValueOnce(save.promise);
	return save;
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
	it("reads again on foreground while nothing is loaded, and stops once something is", async () => {
		fake.loads.push(null, pairing);
		const h = await mount();
		expect(h.value).toMatchObject({ pairing: null, ready: true });
		await act(async () => fake.emit("background"));
		expect(fake.load).toHaveBeenCalledTimes(1);

		await act(async () => fake.emit("active"));
		expect(h.value.pairing).toEqual(pairing);
		expect(fake.load).toHaveBeenCalledTimes(2);

		await act(async () => fake.emit("active"));
		expect(fake.load).toHaveBeenCalledTimes(2);
	});

	it("keeps later changes running after a save fails", async () => {
		const h = await mount();
		fake.save.mockRejectedValueOnce(new Error("locked"));
		await act(async () => {
			await expect(h.value.replace(pairing)).rejects.toThrow("locked");
		});
		expect(h.value.pairing).toBeNull();

		await act(() => h.value.replace(pairing));
		expect(h.value.pairing).toEqual(pairing);
		await act(() => h.value.clear());
		expect(fake.clear).toHaveBeenCalledTimes(1);
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

	describe("currentToken", () => {
		const second = { ...pairing, token: "second" };

		it("is the new pairing's from the moment replace is called", async () => {
			fake.loads.push(pairing);
			const h = await mount();
			expect(h.value.currentToken()).toBe("tok");
			const save = holdSave();
			let replaced: Promise<void> | undefined;
			await act(async () => {
				replaced = h.value.replace(second);
				expect(h.value.currentToken()).toBe("second");
			});
			expect(h.value.pairing).toEqual(pairing);
			await act(async () => {
				save.resolve();
				await replaced;
			});
			expect(h.value.currentToken()).toBe("second");
		});

		it("is the current pairing's again when the save fails", async () => {
			fake.loads.push(pairing);
			const h = await mount();
			const save = holdSave();
			let replaced: Promise<void> | undefined;
			await act(async () => {
				replaced = h.value.replace(second);
			});
			await act(async () => {
				save.reject(new Error("locked"));
				await expect(replaced).rejects.toThrow("locked");
			});
			expect(h.value.currentToken()).toBe("tok");
		});
	});

	describe("clearIfCurrent", () => {
		const replacement = { ...pairing, token: "new-token" };

		it("clears, after its first step, when the token is the pairing's", async () => {
			fake.loads.push(pairing);
			const h = await mount();
			const steps: string[] = [];
			fake.clear.mockImplementationOnce(async () => {
				steps.push("clear");
			});
			let cleared = false;
			await act(async () => {
				cleared = await h.value.clearIfCurrent("tok", async () => {
					steps.push("first");
				});
			});
			expect(cleared).toBe(true);
			expect(steps).toEqual(["first", "clear"]);
			expect(h.value.pairing).toBeNull();
		});

		type Case = [
			string,
			typeof pairing | null,
			typeof pairing | null,
			string | null,
			boolean,
		];
		it.each<Case>([
			// A token nobody recorded stands for the pairing loaded at launch.
			["the launch pairing, no token", pairing, null, null, true],
			[
				"a pairing made in this process, no token",
				pairing,
				replacement,
				null,
				false,
			],
			["nothing loaded at launch, no token", null, pairing, null, false],
			["a pairing that replaced the token", replacement, null, "tok", false],
			["no pairing", null, null, null, false],
		])("against %s", async (_, loaded, replaced, token, expected) => {
			if (loaded) fake.loads.push(loaded);
			const h = await mount();
			if (replaced) await act(() => h.value.replace(replaced));
			const first = vi.fn(async () => {});
			let cleared: boolean | undefined;
			await act(async () => {
				cleared = await h.value.clearIfCurrent(token, first);
			});
			expect(cleared).toBe(expected);
			expect(first).toHaveBeenCalledTimes(expected ? 1 : 0);
			expect(fake.clear).toHaveBeenCalledTimes(expected ? 1 : 0);
			expect(h.value.pairing).toEqual(expected ? null : (replaced ?? loaded));
		});

		it("waits for a replace still saving, then compares with the new pairing", async () => {
			fake.loads.push(pairing);
			const h = await mount();
			const save = holdSave();
			let cleared: Promise<boolean> | undefined;
			await act(async () => {
				void h.value.replace(replacement);
				cleared = h.value.clearIfCurrent("tok", async () => {});
			});
			await act(async () => save.resolve());
			expect(await cleared).toBe(false);
			expect(fake.clear).not.toHaveBeenCalled();
			expect(h.value.pairing).toEqual(replacement);
		});
	});
});
