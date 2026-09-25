import { beforeEach, describe, expect, it, vi } from "vitest";

/**
 * The keychain as a map, recording the accessibility option every call
 * passes: the pairing must be readable during a background relaunch on a
 * locked phone, which the default `WHEN_UNLOCKED` is not.
 */
const keychain = vi.hoisted(() => {
	const items = new Map<string, string>();
	const options: unknown[] = [];
	return {
		items,
		options,
		AFTER_FIRST_UNLOCK_THIS_DEVICE_ONLY: "afterFirstUnlockThisDeviceOnly",
		getItemAsync: vi.fn(async (key: string, opts?: unknown) => {
			options.push(opts);
			return items.get(key) ?? null;
		}),
		setItemAsync: vi.fn(async (key: string, value: string, opts?: unknown) => {
			options.push(opts);
			items.set(key, value);
		}),
		deleteItemAsync: vi.fn(async (key: string, opts?: unknown) => {
			options.push(opts);
			items.delete(key);
		}),
	};
});

vi.mock("expo-secure-store", () => keychain);
vi.mock("expo-constants", () => ({
	default: { deviceName: "Nicolai's iPhone" },
}));
vi.mock("expo-crypto", () => ({ randomUUID: () => "device-uuid" }));

import {
	deviceIdentity,
	KEYCHAIN_OPTIONS,
	type Pairing,
	pairingStore,
} from "./pairing-store";

const pairing: Pairing = {
	mac: {
		macID: "0f8fad5b-d9cb-469f-a165-70867728950e",
		macName: "Studio",
		fingerprint: Buffer.alloc(32, 1).toString("base64"),
		pairedAt: "2026-09-25T10:00:00.000Z",
	},
	token: "tok",
};

beforeEach(() => {
	keychain.items.clear();
	keychain.options.length = 0;
	keychain.getItemAsync.mockClear();
	keychain.setItemAsync.mockClear();
	keychain.deleteItemAsync.mockClear();
});

describe("pairingStore", () => {
	it("keeps the Mac record and the token in one keychain item", async () => {
		await pairingStore.save(pairing);
		expect([...keychain.items.keys()]).toEqual(["steno.pairing.v1"]);
		expect(JSON.parse(keychain.items.get("steno.pairing.v1") ?? "")).toEqual(
			pairing,
		);
		expect(await pairingStore.load()).toEqual(pairing);
	});

	it("reads and writes every item as available after the first unlock", async () => {
		expect(KEYCHAIN_OPTIONS).toEqual({
			keychainAccessible: keychain.AFTER_FIRST_UNLOCK_THIS_DEVICE_ONLY,
		});
		await pairingStore.save(pairing);
		await pairingStore.load();
		await deviceIdentity();
		await pairingStore.clear();
		expect(keychain.options.length).toBeGreaterThanOrEqual(5);
		for (const passed of keychain.options) {
			expect(passed).toEqual(KEYCHAIN_OPTIONS);
		}
	});

	it("returns null for a missing, damaged or tokenless item", async () => {
		expect(await pairingStore.load()).toBeNull();
		keychain.items.set("steno.pairing.v1", "{not json");
		expect(await pairingStore.load()).toBeNull();
		keychain.items.set(
			"steno.pairing.v1",
			JSON.stringify({ mac: pairing.mac, token: "" }),
		);
		expect(await pairingStore.load()).toBeNull();
		keychain.items.set(
			"steno.pairing.v1",
			JSON.stringify({ mac: { macID: "x" }, token: "tok" }),
		);
		expect(await pairingStore.load()).toBeNull();
	});

	it("clear removes the item so load is null afterwards", async () => {
		await pairingStore.save(pairing);
		await pairingStore.clear();
		expect(keychain.items.size).toBe(0);
		expect(await pairingStore.load()).toBeNull();
	});
});

describe("deviceIdentity", () => {
	it("mints the device id once and keeps it", async () => {
		const first = await deviceIdentity();
		const second = await deviceIdentity();
		expect(first).toEqual({
			deviceID: "device-uuid",
			deviceName: "Nicolai's iPhone",
		});
		expect(second).toEqual(first);
		expect(keychain.setItemAsync).toHaveBeenCalledTimes(1);
	});
});
