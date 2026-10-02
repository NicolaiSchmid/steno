import { describe, expect, it } from "vitest";
import { parseHash } from "./app";

describe("parseHash", () => {
	it("serves the main window when the hash is empty", () => {
		expect(parseHash("").path).toBe("/main");
		expect(parseHash("#").path).toBe("/main");
		expect(parseHash("#/main").path).toBe("/main");
	});

	it("reads the panel routes with their request", () => {
		expect(parseHash("#/panel/bubble").path).toBe("/panel/bubble");
		const prompt = parseHash("#/panel/prompt?app=Microsoft+Teams&seconds=60");
		expect(prompt.path).toBe("/panel/prompt");
		expect(prompt.params.get("app")).toBe("Microsoft Teams");
		expect(prompt.params.get("seconds")).toBe("60");
	});

	it("keeps the stories and the query flags", () => {
		const route = parseHash("#/stories?dark");
		expect(route.path).toBe("/stories");
		expect(route.params.has("dark")).toBe(true);
		const main = parseHash("#/main?tab=transcript&picker&scenario=failed");
		expect(main.params.get("tab")).toBe("transcript");
		expect(main.params.has("picker")).toBe(true);
		expect(main.params.get("scenario")).toBe("failed");
	});
});
