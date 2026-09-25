/**
 * The runtime gate (plan decision 10): iOS 18 before 18.6 had a Local Network
 * privilege state-sync bug (FB14321888), so the recorder refuses to run there
 * and shows `UpdateIOSScreen` instead. Pure; `Platform.Version` is a string
 * on iOS ("18.6.1"), a number on Android.
 */
export const MINIMUM_IOS_VERSION = "18.6";

export function compareVersions(a: string, b: string): number {
	const pa = a.split(".").map((p) => Number.parseInt(p, 10) || 0);
	const pb = b.split(".").map((p) => Number.parseInt(p, 10) || 0);
	const length = Math.max(pa.length, pb.length);
	for (let i = 0; i < length; i++) {
		const diff = (pa[i] ?? 0) - (pb[i] ?? 0);
		if (diff !== 0) return diff < 0 ? -1 : 1;
	}
	return 0;
}

export function isSupportedIOS(
	version: string | number,
	minimum: string = MINIMUM_IOS_VERSION,
): boolean {
	const text = String(version).trim();
	if (!/^\d+(\.\d+)*$/.test(text)) return false;
	return compareVersions(text, minimum) >= 0;
}
