import { describe, expect, it } from "vitest";
import { cn } from "./cn";

describe("cn", () => {
	it("joins strings, arrays and conditional objects", () => {
		expect(cn("a", ["b", { c: true, d: false }], undefined, null)).toBe(
			"a b c",
		);
	});

	it("lets the last Tailwind class win on conflict", () => {
		expect(cn("px-2 py-1", "px-4")).toBe("py-1 px-4");
		expect(cn("bg-card", "bg-accent")).toBe("bg-accent");
	});

	it("keeps unrelated utilities", () => {
		expect(cn("h-8 rounded-control", "w-full")).toBe(
			"h-8 rounded-control w-full",
		);
	});
});
