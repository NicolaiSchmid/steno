import { describe, expect, it } from "vitest";
import { createFormatter } from "../main/format";
import {
	deviceName,
	folderUsageText,
	formatBytes,
	pairedText,
	pairingExpiryText,
	retentionTitle,
	transferProgress,
	updateStatusText,
} from "./settings-format";

const NOW = new Date("2026-09-29T12:50:00.000Z");
const formatter = createFormatter({ locale: "en-US", timeZone: "UTC" });

describe("settings-format", () => {
	it("formats sizes the way the Mac does", () => {
		expect(formatBytes(734_003_200)).toBe("734 MB");
		expect(formatBytes(4_200_000_000)).toBe("4.2 GB");
		expect(formatBytes(22_000_000)).toBe("22 MB");
		expect(formatBytes(512_000)).toBe("512 KB");
		expect(formatBytes(12)).toBe("12 B");
	});

	it("words the folder usage by its state", () => {
		expect(folderUsageText({ folderUsage: "measuring" })).toBe("Measuring…");
		expect(folderUsageText({ folderUsage: "unavailable" })).toBe(
			"Size unavailable",
		);
		expect(
			folderUsageText({ folderUsage: "measured", folderUsageBytes: 4_200 }),
		).toBe("Recordings use 4 KB");
	});

	it("labels the retention options", () => {
		expect(retentionTitle("keepForever", 30)).toBe("Forever");
		expect(retentionTitle("keepDays", 14)).toBe("For 14 days");
		expect(retentionTitle("keepDays", 1)).toBe("For 1 day");
		expect(retentionTitle("deleteAfterProcessing", 30)).toBe(
			"Until processed, then delete",
		);
	});

	it("words the update status", () => {
		const base = {
			canCheck: true,
			automaticallyChecks: true,
			automaticallyDownloads: false,
		};
		expect(updateStatusText({ ...base, outcome: "notChecked" }, NOW)).toBe(
			"Not checked yet",
		);
		expect(
			updateStatusText(
				{
					...base,
					outcome: "upToDate",
					lastCheckAt: "2026-09-29T10:50:00.000Z",
				},
				NOW,
				formatter,
			),
		).toBe("Up to date, checked 2 hr. ago");
		expect(
			updateStatusText({ ...base, outcome: "available", detail: "0.9.1" }, NOW),
		).toBe("Update available: 0.9.1");
		expect(updateStatusText({ ...base, outcome: "failed" }, NOW)).toBe(
			"Could not check for updates",
		);
	});

	it("words a paired phone and the pairing expiry", () => {
		const device = {
			id: "00000000-0000-0000-0000-000000000028",
			name: "Nicolai's iPhone",
			pairedAt: "2026-09-24T12:50:00.000Z",
			lastSeenAt: "2026-09-29T12:48:00.000Z",
		};
		expect(pairedText(device, NOW, formatter)).toBe(
			"Paired Sep 24 · last seen Sep 29, 12:48",
		);
		const { lastSeenAt: _seen, ...unseen } = device;
		expect(pairedText(unseen, NOW, formatter)).toBe("Paired Sep 24");
		expect(pairingExpiryText("2026-09-29T12:54:00.000Z", NOW)).toBe(
			"It expires in 4 minutes.",
		);
		expect(pairingExpiryText("2026-09-29T12:51:30.000Z", NOW)).toBe(
			"It expires in 2 minutes.",
		);
		expect(pairingExpiryText("2026-09-29T12:50:30.000Z", NOW)).toBe(
			"It expires in a minute.",
		);
		expect(pairingExpiryText("2026-09-29T12:49:00.000Z", NOW)).toBeNull();
	});

	it("measures a transfer and names its phone", () => {
		const receipt = {
			deviceID: "00000000-0000-0000-0000-000000000028",
			recordingID: "00000000-0000-0000-0000-000000000029",
			receivedBytes: 500,
			totalBytes: 1000,
		};
		expect(transferProgress(receipt)).toBe(0.5);
		expect(transferProgress({ ...receipt, receivedBytes: 1200 })).toBe(1);
		const { totalBytes: _total, ...undeclared } = receipt;
		expect(transferProgress(undeclared)).toBe(0);
		const devices = [
			{
				id: receipt.deviceID,
				name: "Nicolai's iPhone",
				pairedAt: "2026-09-24T12:50:00.000Z",
			},
		];
		expect(deviceName({ devices }, receipt.deviceID)).toBe("Nicolai's iPhone");
		expect(deviceName({ devices }, receipt.recordingID)).toBe("your iPhone");
	});
});
