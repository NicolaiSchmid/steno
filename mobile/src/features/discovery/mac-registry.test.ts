import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createMacRegistry, EMPTY_REGISTRY, findByMacID } from "./mac-registry";

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
		registry.found(studio);
		registry.found(laptop);
		registry.found({ ...studio, macID: null });
		expect(registry.snapshot().services).toEqual([
			laptop,
			{ ...studio, macID: null },
		]);
		registry.lost(laptop);
		expect(registry.snapshot().services).toEqual([{ ...studio, macID: null }]);
	});

	it("finds by mac id case-insensitively and ignores services without one", () => {
		const services = [{ name: "Anon", macID: null }, studio];
		expect(findByMacID(services, studio.macID.toUpperCase())).toEqual(studio);
		expect(findByMacID(services, laptop.macID)).toBeNull();
	});

	it("notifies subscribers only on change and keeps snapshots immutable", () => {
		const registry = createMacRegistry();
		const listener = vi.fn();
		const unsubscribe = registry.subscribe(listener);
		const before = registry.snapshot();
		registry.lost(studio);
		expect(listener).not.toHaveBeenCalled();
		registry.browserState({ state: "ready", policyDenied: false });
		expect(listener).toHaveBeenCalledTimes(1);
		expect(registry.snapshot()).not.toBe(before);
		expect(registry.snapshot().browser).toEqual({
			state: "ready",
			policyDenied: false,
		});
		unsubscribe();
		registry.reset();
		expect(listener).toHaveBeenCalledTimes(1);
		expect(registry.snapshot()).toBe(EMPTY_REGISTRY);
	});
});

describe("waitForMac", () => {
	beforeEach(() => vi.useFakeTimers());
	afterEach(() => vi.useRealTimers());

	it("resolves immediately when the Mac is already known", async () => {
		const registry = createMacRegistry();
		registry.found(studio);
		await expect(registry.waitForMac(studio.macID, 1000)).resolves.toEqual(
			studio,
		);
	});

	it("resolves when the Mac appears later and stops listening afterwards", async () => {
		const registry = createMacRegistry();
		const pending = registry.waitForMac(studio.macID, 5000);
		registry.found(laptop);
		registry.found(studio);
		await expect(pending).resolves.toEqual(studio);
		vi.advanceTimersByTime(10_000);
	});

	it("rejects after the timeout", async () => {
		const registry = createMacRegistry();
		const pending = registry.waitForMac(studio.macID, 5000);
		const outcome = expect(pending).rejects.toThrow(/not found/);
		vi.advanceTimersByTime(5000);
		await outcome;
		registry.found(studio);
	});
});
