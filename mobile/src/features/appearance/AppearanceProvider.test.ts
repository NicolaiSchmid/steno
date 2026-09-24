import { describe, expect, it } from "vitest";

import { parseAppearancePreference } from "./appearancePreference";

describe("parseAppearancePreference", () => {
	it("accepts the two explicit themes", () => {
		expect(parseAppearancePreference("light")).toBe("light");
		expect(parseAppearancePreference("dark")).toBe("dark");
	});

	it("falls back to system for anything else", () => {
		expect(parseAppearancePreference(null)).toBe("system");
		expect(parseAppearancePreference("")).toBe("system");
		expect(parseAppearancePreference("sepia")).toBe("system");
	});
});
