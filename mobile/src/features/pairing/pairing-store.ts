import Constants from "expo-constants";
import { randomUUID } from "expo-crypto";
import * as SecureStore from "expo-secure-store";

/**
 * The one pairing the phone keeps (several Macs per phone is a non-goal):
 * the Mac's identity in a small JSON record and the bearer token, both in
 * the keychain through expo-secure-store. Plus the phone's own stable
 * device id, minted once per install.
 */
export type PairedMac = {
	macID: string;
	macName: string;
	/** Standard base64 of the leaf fingerprint. */
	fingerprint: string;
	pairedAt: string;
};

export type Pairing = { mac: PairedMac; token: string };

const MAC_KEY = "steno.pairing.mac.v1";
const TOKEN_KEY = "steno.pairing.token.v1";
const DEVICE_ID_KEY = "steno.device.id.v1";

function parsePairedMac(text: string | null): PairedMac | null {
	if (!text) return null;
	try {
		const raw = JSON.parse(text) as Partial<PairedMac>;
		if (
			typeof raw.macID === "string" &&
			typeof raw.macName === "string" &&
			typeof raw.fingerprint === "string" &&
			typeof raw.pairedAt === "string"
		) {
			return {
				macID: raw.macID,
				macName: raw.macName,
				fingerprint: raw.fingerprint,
				pairedAt: raw.pairedAt,
			};
		}
	} catch {
		// fall through
	}
	return null;
}

export const pairingStore = {
	async load(): Promise<Pairing | null> {
		const [macText, token] = await Promise.all([
			SecureStore.getItemAsync(MAC_KEY),
			SecureStore.getItemAsync(TOKEN_KEY),
		]);
		const mac = parsePairedMac(macText);
		if (!mac || !token) return null;
		return { mac, token };
	},

	async save(mac: PairedMac, token: string): Promise<void> {
		await SecureStore.setItemAsync(MAC_KEY, JSON.stringify(mac));
		await SecureStore.setItemAsync(TOKEN_KEY, token);
	},

	async clear(): Promise<void> {
		await SecureStore.deleteItemAsync(TOKEN_KEY);
		await SecureStore.deleteItemAsync(MAC_KEY);
	},
};

export type DeviceIdentity = { deviceID: string; deviceName: string };

export async function deviceIdentity(): Promise<DeviceIdentity> {
	let deviceID = await SecureStore.getItemAsync(DEVICE_ID_KEY);
	if (!deviceID) {
		deviceID = randomUUID();
		await SecureStore.setItemAsync(DEVICE_ID_KEY, deviceID);
	}
	return { deviceID, deviceName: Constants.deviceName ?? "iPhone" };
}
