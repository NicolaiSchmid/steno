import type {
	GeneralSettingsSnapshot,
	PhoneSettingsSnapshot,
	RecordingSettingsSnapshot,
} from "@/bridge/contract";
import { type Formatter, format } from "../main/format";

/**
 * The sentences and figures the Settings sections show that are not spelled
 * by the host: sizes, relative times and the retention picker's labels. The
 * wording follows the SwiftUI window's so the tests over the view models
 * and these read alike.
 */

/** `734 MB`, `4.2 GB`, `22 MB`, `512 KB`. */
export function formatBytes(bytes: number): string {
	const abs = Math.abs(bytes);
	if (abs >= 1e9) {
		const gb = bytes / 1e9;
		return `${gb >= 10 ? Math.round(gb) : gb.toFixed(1)} GB`;
	}
	if (abs >= 1e6) {
		return `${Math.round(bytes / 1e6)} MB`;
	}
	if (abs >= 1e3) {
		return `${Math.round(bytes / 1e3)} KB`;
	}
	return `${Math.round(bytes)} B`;
}

/** "Measuring…", "Recordings use 734 MB" or "Size unavailable". */
export function folderUsageText(
	snapshot: Pick<RecordingSettingsSnapshot, "folderUsage" | "folderUsageBytes">,
): string {
	switch (snapshot.folderUsage) {
		case "measuring":
			return "Measuring…";
		case "unavailable":
			return "Size unavailable";
		case "measured":
			return `Recordings use ${formatBytes(snapshot.folderUsageBytes ?? 0)}`;
	}
}

/** The retention picker's option label; the days option shows the number. */
export function retentionTitle(
	mode: RecordingSettingsSnapshot["retention"]["mode"],
	days: number,
): string {
	switch (mode) {
		case "keepForever":
			return "Forever";
		case "keepDays":
			return `For ${days} ${days === 1 ? "day" : "days"}`;
		case "deleteAfterProcessing":
			return "Until processed, then delete";
	}
}

/**
 * "Up to date, checked 2 hr. ago", "Update available: 0.9.1", "Could not
 * check for updates" or "Not checked yet".
 */
export function updateStatusText(
	updates: GeneralSettingsSnapshot["updates"],
	now: Date = new Date(),
	formatter: Formatter = format,
): string {
	switch (updates.outcome) {
		case "notChecked":
			return updates.lastCheckAt
				? `Checked ${formatter.relative(updates.lastCheckAt, now)}`
				: "Not checked yet";
		case "upToDate":
			return updates.lastCheckAt
				? `Up to date, checked ${formatter.relative(updates.lastCheckAt, now)}`
				: "Up to date";
		case "available":
			return updates.detail
				? `Update available: ${updates.detail}`
				: "Update available";
		case "failed":
			return "Could not check for updates";
	}
}

/** "Paired Sep 24 · last seen Sep 29, 14:48". */
export function pairedText(
	device: PhoneSettingsSnapshot["devices"][number],
	now: Date = new Date(),
	formatter: Formatter = format,
): string {
	let text = `Paired ${formatter.shortDate(device.pairedAt, now)}`;
	if (device.lastSeenAt) {
		text += ` · last seen ${formatter.shortDate(device.lastSeenAt, now)}, ${formatter.time(device.lastSeenAt)}`;
	}
	return text;
}

/** "It expires in 4 minutes." or "It expires in a minute."; null once past. */
export function pairingExpiryText(
	expiresAt: string,
	now: Date = new Date(),
): string | null {
	const seconds = (new Date(expiresAt).getTime() - now.getTime()) / 1000;
	if (seconds <= 0) {
		return null;
	}
	const minutes = Math.ceil(seconds / 60);
	return minutes <= 1
		? "It expires in a minute."
		: `It expires in ${minutes} minutes.`;
}

/** How much of a transfer has arrived, 0 to 1; 0 without a declared size. */
export function transferProgress(
	receipt: PhoneSettingsSnapshot["receipts"][number],
): number {
	if (!receipt.totalBytes || receipt.totalBytes <= 0) {
		return 0;
	}
	return Math.min(1, receipt.receivedBytes / receipt.totalBytes);
}

/** The phone a transfer comes from, by name; "your iPhone" when unknown. */
export function deviceName(
	snapshot: Pick<PhoneSettingsSnapshot, "devices">,
	deviceID: string,
): string {
	return (
		snapshot.devices.find((device) => device.id === deviceID)?.name ??
		"your iPhone"
	);
}
