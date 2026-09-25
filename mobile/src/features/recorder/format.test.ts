import { describe, expect, it } from "vitest";

import {
	formatBytes,
	formatDuration,
	formatStartedAt,
	uploadPercent,
} from "./format";

describe("formatDuration", () => {
	it("formats minutes and hours", () => {
		expect(formatDuration(0)).toBe("0:00");
		expect(formatDuration(59.9)).toBe("0:59");
		expect(formatDuration(65)).toBe("1:05");
		expect(formatDuration(3600)).toBe("1:00:00");
		expect(formatDuration(3725)).toBe("1:02:05");
	});

	it("reads 0:00 for garbage", () => {
		expect(formatDuration(-5)).toBe("0:00");
		expect(formatDuration(Number.NaN)).toBe("0:00");
	});
});

describe("formatBytes", () => {
	it("uses kB below a megabyte and MB above", () => {
		expect(formatBytes(0)).toBe("0 kB");
		expect(formatBytes(512_000)).toBe("512 kB");
		expect(formatBytes(1_500_000)).toBe("1.5 MB");
		expect(formatBytes(29_000_000)).toBe("29.0 MB");
		expect(formatBytes(123_456_789)).toBe("123 MB");
		expect(formatBytes(-1)).toBe("0 kB");
	});
});

describe("uploadPercent", () => {
	it("counts uploaded chunks plus the bytes in flight, clamped", () => {
		expect(uploadPercent(0, 0, 16, 0)).toBe(0);
		expect(uploadPercent(100, 0, 40, 0)).toBe(0);
		expect(uploadPercent(100, 1, 40, 0)).toBe(40);
		expect(uploadPercent(100, 1, 40, 30)).toBe(70);
		expect(uploadPercent(100, 3, 40, 0)).toBe(100);
		expect(uploadPercent(100, 0, 40, -5)).toBe(0);
	});
});

describe("formatStartedAt", () => {
	const now = new Date(2026, 8, 25, 15, 0, 0);
	const local = (y: number, m: number, d: number, h: number, min: number) =>
		new Date(y, m, d, h, min).toISOString();

	it("says Today and Yesterday, else a short date", () => {
		expect(formatStartedAt(local(2026, 8, 25, 14, 5), now)).toMatch(/^Today /);
		expect(formatStartedAt(local(2026, 8, 24, 23, 59), now)).toMatch(
			/^Yesterday /,
		);
		expect(formatStartedAt(local(2026, 8, 20, 9, 30), now)).not.toMatch(
			/Today|Yesterday/,
		);
		expect(formatStartedAt(local(2026, 8, 20, 9, 30), now)).toMatch(/Sep/);
	});

	it("is empty for an invalid date", () => {
		expect(formatStartedAt("nope", now)).toBe("");
	});
});
