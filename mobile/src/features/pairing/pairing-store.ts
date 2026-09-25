import Constants from "expo-constants";
import { randomUUID } from "expo-crypto";
import * as SecureStore from "expo-secure-store";

/**
 * The one pairing the phone keeps (several Macs per phone is a non-goal):
 * the Mac's identity and the bearer token in one JSON keychain item, so a
 * crash mid-save can never leave a new Mac with an old token. Plus the
 * phone's own stable device id, minted once per install.
 *
 * Every item is readable after the first unlock: iOS relaunches the app in
 * the background to finish uploads while the phone is locked, and the
 * default `WHEN_UNLOCKED` would make the pairing unreadable exactly then.
 */
export type PairedMac = {
	macID: string;
	macName: string;
	/** Standard base64 of the leaf fingerprint. */
	fingerprint: string;
	pairedAt: string;
};

export type Pairing = { mac: PairedMac; token: string };

const PAIRING_KEY = "steno.pairing.v1";
const DEVICE_ID_KEY = "steno.device.id.v1";

export const KEYCHAIN_OPTIONS: SecureStore.SecureStoreOptions = {
	keychainAccessible: SecureStore.AFTER_FIRST_UNLOCK_THIS_DEVICE_ONLY,
};

function parsePairing(text: string | null): Pairing | null {
	if (!text) return null;
	try {
		const raw = JSON.parse(text) as {
			mac?: Partial<PairedMac>;
			token?: unknown;
		};
		const mac = raw.mac;
		if (
			mac &&
			typeof mac.macID === "string" &&
			typeof mac.macName === "string" &&
			typeof mac.fingerprint === "string" &&
			typeof mac.pairedAt === "string" &&
			typeof raw.token === "string" &&
			raw.token.length > 0
		) {
			return {
				mac: {
					macID: mac.macID,
					macName: mac.macName,
					fingerprint: mac.fingerprint,
					pairedAt: mac.pairedAt,
				},
				token: raw.token,
			};
		}
	} catch {
		// fall through
	}
	return null;
}

export const pairingStore = {
	async load(): Promise<Pairing | null> {
		return parsePairing(
			await SecureStore.getItemAsync(PAIRING_KEY, KEYCHAIN_OPTIONS),
		);
	},

	async save(pairing: Pairing): Promise<void> {
		await SecureStore.setItemAsync(
			PAIRING_KEY,
			JSON.stringify(pairing),
			KEYCHAIN_OPTIONS,
		);
	},

	async clear(): Promise<void> {
		await SecureStore.deleteItemAsync(PAIRING_KEY, KEYCHAIN_OPTIONS);
	},
};

export type DeviceIdentity = { deviceID: string; deviceName: string };

export async function deviceIdentity(): Promise<DeviceIdentity> {
	let deviceID = await SecureStore.getItemAsync(
		DEVICE_ID_KEY,
		KEYCHAIN_OPTIONS,
	);
	if (!deviceID) {
		deviceID = randomUUID();
		await SecureStore.setItemAsync(DEVICE_ID_KEY, deviceID, KEYCHAIN_OPTIONS);
	}
	return { deviceID, deviceName: Constants.deviceName ?? "iPhone" };
}
