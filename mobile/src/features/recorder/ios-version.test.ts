import { describe, expect, it } from "vitest";

import { compareVersions, isSupportedIOS } from "./ios-version";

describe("compareVersions", () => {
	it("compares numerically per component", () => {
		expect(compareVersions("18.6", "18.6")).toBe(0);
		expect(compareVersions("18.6.0", "18.6")).toBe(0);
		expect(compareVersions("18.10", "18.9")).toBe(1);
		expect(compareVersions("17.9", "18.0")).toBe(-1);
		expect(compareVersions("26", "18.6")).toBe(1);
	});
});

describe("isSupportedIOS", () => {
	it("accepts 18.6 and later", () => {
		expect(isSupportedIOS("18.6")).toBe(true);
		expect(isSupportedIOS("18.6.1")).toBe(true);
		expect(isSupportedIOS("18.7")).toBe(true);
		expect(isSupportedIOS("19.0")).toBe(true);
		expect(isSupportedIOS("26.0")).toBe(true);
	});

	it("rejects 18.5 and older and anything unparseable", () => {
		expect(isSupportedIOS("18.5.1")).toBe(false);
		expect(isSupportedIOS("18")).toBe(false);
		expect(isSupportedIOS("17.7")).toBe(false);
		expect(isSupportedIOS("")).toBe(false);
		expect(isSupportedIOS("beta")).toBe(false);
	});

	it("accepts a numeric version (Android shape) by string conversion", () => {
		expect(isSupportedIOS(19)).toBe(true);
		expect(isSupportedIOS(18)).toBe(false);
	});
});
