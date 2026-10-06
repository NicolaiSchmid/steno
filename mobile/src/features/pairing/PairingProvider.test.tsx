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

	it("runs a clear after a replace still saving", async () => {
		const h = await mount();
		const save = holdSave();
		let cleared: Promise<void> | undefined;
		await act(async () => {
			void h.value.replace(pairing);
			cleared = h.value.clear();
		});
		await act(async () => save.resolve());
		await act(async () => cleared);
		expect(fake.clear).toHaveBeenCalledTimes(1);
		expect(h.value.pairing).toBeNull();
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
		const third = { ...pairing, token: "third" };

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

		it("stays the last replace's while an earlier one finishes", async () => {
			const h = await mount();
			const save = holdSave();
			const lastSave = holdSave();
			let last: Promise<void> | undefined;
			await act(async () => {
				void h.value.replace(second);
				last = h.value.replace(third);
			});
			await act(async () => save.resolve());
			expect(h.value.pairing).toEqual(second);
			expect(h.value.currentToken()).toBe("third");
			await act(async () => {
				lastSave.resolve();
				await last;
			});
			expect(h.value.pairing).toEqual(third);
			expect(h.value.currentToken()).toBe("third");
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

		it("treats a token nobody recorded as the keychain's", async () => {
			fake.loads.push(pairing);
			const h = await mount();
			await act(async () => {
				await h.value.clearIfCurrent(null, async () => {});
			});
			expect(fake.clear).toHaveBeenCalledTimes(1);
			expect(h.value.pairing).toBeNull();
		});

		it("keeps a pairing made in this process against a token nobody recorded", async () => {
			fake.loads.push(pairing);
			const h = await mount();
			await act(() => h.value.replace(replacement));
			let cleared = true;
			await act(async () => {
				cleared = await h.value.clearIfCurrent(null, async () => {});
			});
			expect(cleared).toBe(false);
			expect(fake.clear).not.toHaveBeenCalled();
			expect(h.value.pairing).toEqual(replacement);
		});

		it("matches nothing with a token nobody recorded when nothing was loaded at launch", async () => {
			const h = await mount();
			await act(() => h.value.replace(pairing));
			let cleared = true;
			await act(async () => {
				cleared = await h.value.clearIfCurrent(null, async () => {});
			});
			expect(cleared).toBe(false);
			expect(h.value.pairing).toEqual(pairing);
		});

		it("keeps a pairing that replaced the token, and runs nothing", async () => {
			fake.loads.push(replacement);
			const h = await mount();
			const first = vi.fn(async () => {});
			let cleared = true;
			await act(async () => {
				cleared = await h.value.clearIfCurrent("tok", first);
			});
			expect(cleared).toBe(false);
			expect(first).not.toHaveBeenCalled();
			expect(fake.clear).not.toHaveBeenCalled();
			expect(h.value.pairing).toEqual(replacement);
		});

		it("does nothing without a pairing", async () => {
			const h = await mount();
			const first = vi.fn(async () => {});
			await act(async () => {
				expect(await h.value.clearIfCurrent(null, first)).toBe(false);
			});
			expect(first).not.toHaveBeenCalled();
			expect(fake.clear).not.toHaveBeenCalled();
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
