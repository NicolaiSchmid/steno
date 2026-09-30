import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { bridgeTopics, fixtureSchemas, methodParams } from "./contract";

/**
 * The Swift side (`Sources/StenoBridge`, `BridgeFixturesTests`) writes one
 * JSON fixture per contract type into `fixtures/bridge/`. Every fixture must
 * parse with its schema here and every schema must have a fixture, so a
 * change on either side that the other has not seen fails this test.
 */
const fixturesDir = join(__dirname, "..", "..", "fixtures", "bridge");
const index = JSON.parse(
	readFileSync(join(fixturesDir, "index.json"), "utf8"),
) as string[];

describe("bridge contract fixtures", () => {
	it("lists every fixture file on disk in index.json", () => {
		const files = readdirSync(fixturesDir)
			.filter((name) => name.endsWith(".json") && name !== "index.json")
			.map((name) => name.replace(/\.json$/, ""))
			.sort();
		expect(files).toEqual([...index].sort());
	});

	it("has a schema for every recorded fixture and a fixture for every schema", () => {
		expect([...index].sort()).toEqual(Object.keys(fixtureSchemas).sort());
	});

	it.each(index)("parses %s with its schema", (name) => {
		const schema = fixtureSchemas[name as keyof typeof fixtureSchemas];
		expect(schema, `no schema for ${name}`).toBeDefined();
		const json = JSON.parse(
			readFileSync(join(fixturesDir, `${name}.json`), "utf8"),
		);
		const result = schema.safeParse(json);
		expect(
			result.success,
			JSON.stringify(result.success ? null : result.error.issues, null, 2),
		).toBe(true);
	});

	it("records a snapshot fixture for every topic", () => {
		for (const topic of bridgeTopics) {
			expect(index, `no fixture for topic ${topic}`).toContain(topic);
		}
	});

	it("declares params for every method", () => {
		const request = JSON.parse(
			readFileSync(join(fixturesDir, "envelope.request.json"), "utf8"),
		);
		expect(Object.keys(methodParams)).toContain(request.method);
	});
});
