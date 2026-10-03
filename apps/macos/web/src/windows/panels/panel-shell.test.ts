import { describe, expect, it } from "vitest";
import { deviceSize } from "./panel-shell";

describe("deviceSize", () => {
	it("scales CSS pixels by the device pixel ratio", () => {
		expect(deviceSize({ width: 79.6, height: 41.6 }, 1)).toEqual({
			width: 79.6,
			height: 41.6,
		});
		// WebKitGTK at 120 dpi: the window must be a quarter larger.
		expect(deviceSize({ width: 80, height: 42 }, 1.25)).toEqual({
			width: 100,
			height: 52.5,
		});
		expect(deviceSize({ width: 80, height: 42 }, 2)).toEqual({
			width: 160,
			height: 84,
		});
	});

	it("reads a missing or broken ratio as 1 and an empty box as no size", () => {
		for (const ratio of [0, -1, Number.NaN, Number.POSITIVE_INFINITY]) {
			expect(deviceSize({ width: 80, height: 42 }, ratio)).toEqual({
				width: 80,
				height: 42,
			});
		}
		expect(deviceSize({ width: 0, height: 42 }, 1)).toBeUndefined();
		expect(deviceSize({ width: 80, height: 0 }, 1)).toBeUndefined();
	});
});
