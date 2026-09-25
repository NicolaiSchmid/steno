import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createMacRegistry, EMPTY_REGISTRY } from "./mac-registry";

const studio = {
	name: "Studio",
	macID: "0f8fad5b-d9cb-469f-a165-70867728950e",
};
const laptop = {
	name: "Laptop",
	macID: "7c9e6679-7425-40de-944b-e07fc1f90ae7",
};

describe("createMacRegistry", () => {
	it("starts empty and tracks found, replaced and lost services by name", () => {
		const registry = createMacRegistry();
		expect(registry.snapshot()).toBe(EMPTY_REGISTRY);
		registry.apply({ type: "found", service: studio });
		registry.apply({ type: "found", service: laptop });
		registry.apply({ type: "found", service: { ...studio, macID: null } });
		expect(registry.snapshot().services).toEqual([
			laptop,
			{ ...studio, macID: null },
		]);
		registry.apply({ type: "lost", service: laptop });
		expect(registry.snapshot().services).toEqual([{ ...studio, macID: null }]);
	});

	it("finds by mac id case-insensitively and ignores services without one", () => {
		const registry = createMacRegistry();
		registry.apply({ type: "found", service: { name: "Anon", macID: null } });
		registry.apply({ type: "found", service: studio });
		expect(registry.findByMacID(studio.macID.toUpperCase())).toEqual(studio);
		expect(registry.findByMacID(laptop.macID)).toBeNull();
	});

	it("notifies subscribers only on change and keeps snapshots immutable", () => {
		const registry = createMacRegistry();
		const listener = vi.fn();
		const unsubscribe = registry.subscribe(listener);
		const before = registry.snapshot();
		registry.apply({ type: "lost", service: studio });
		expect(listener).not.toHaveBeenCalled();
		registry.apply({
			type: "state",
			state: { state: "ready", policyDenied: false },
		});
		expect(listener).toHaveBeenCalledTimes(1);
		expect(registry.snapshot()).not.toBe(before);
		expect(registry.snapshot().browser).toEqual({
			state: "ready",
			policyDenied: false,
		});
		unsubscribe();
		registry.apply({ type: "reset" });
		expect(listener).toHaveBeenCalledTimes(1);
		expect(registry.snapshot()).toBe(EMPTY_REGISTRY);
	});
});

describe("waitForMac", () => {
	beforeEach(() => vi.useFakeTimers());
	afterEach(() => vi.useRealTimers());

	it("resolves immediately when the Mac is already known", async () => {
		const registry = createMacRegistry();
		registry.apply({ type: "found", service: studio });
		await expect(registry.waitForMac(studio.macID, 1000)).resolves.toEqual(
			studio,
		);
	});

	it("resolves when the Mac appears later and stops listening afterwards", async () => {
		const registry = createMacRegistry();
		const pending = registry.waitForMac(studio.macID, 5000);
		registry.apply({ type: "found", service: laptop });
		registry.apply({ type: "found", service: studio });
		await expect(pending).resolves.toEqual(studio);
		vi.advanceTimersByTime(10_000);
	});

	it("rejects after the timeout", async () => {
		const registry = createMacRegistry();
		const pending = registry.waitForMac(studio.macID, 5000);
		const outcome = expect(pending).rejects.toThrow(/not found/);
		vi.advanceTimersByTime(5000);
		await outcome;
		registry.apply({ type: "found", service: studio });
	});
});
