// @vitest-environment node
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const script = fileURLToPath(
	new URL("./check-ui-restyle.mjs", import.meta.url),
);
const projectRoot = fileURLToPath(new URL("..", import.meta.url));

function runScript(args: string[]): { status: number; output: string } {
	try {
		const output = execFileSync(process.execPath, [script, ...args], {
			cwd: projectRoot,
			encoding: "utf8",
			stdio: ["ignore", "pipe", "pipe"],
		});
		return { status: 0, output };
	} catch (error) {
		const failure = error as { status: number; stderr: string; stdout: string };
		return {
			status: failure.status,
			output: `${failure.stdout}${failure.stderr}`,
		};
	}
}

describe("check-ui-restyle", () => {
	it("reports the restyled class with file and line and exits 1", () => {
		const result = runScript(["--root", "scripts/__fixtures__"]);
		expect(result.status).toBe(1);
		expect(result.output).toContain(
			'scripts/__fixtures__/restyle-violation.tsx:13: <Button> className "bg-destructive"',
		);
		expect(result.output).toContain('"text-[11px]"');
		expect(result.output).toContain('"hover:opacity-50"');
		expect(result.output).not.toContain('"w-full"');
		expect(result.output).not.toContain('"justify-start"');
	});

	it("passes the app's own sources", () => {
		const result = runScript([]);
		expect(result.output).toContain("lint:ui ok");
		expect(result.status).toBe(0);
	});
});

describe("isLayoutClass", async () => {
	const { isLayoutClass } = await import("./check-ui-restyle.mjs");

	it("allows layout classes with variants and negatives", () => {
		for (const token of [
			"w-full",
			"h-9",
			"min-h-0",
			"flex-1",
			"flex-col",
			"grid-cols-2",
			"gap-2",
			"-ml-1.5",
			"ml-auto",
			"md:hidden",
			"data-[open]:absolute",
			"shrink-0",
			"truncate",
			"overflow-hidden",
			"inline-flex",
			"z-50",
		]) {
			expect(isLayoutClass(token), token).toBe(true);
		}
	});

	it("rejects classes that change the look", () => {
		for (const token of [
			"bg-card",
			"text-[13px]",
			"rounded-full",
			"p-4",
			"px-2",
			"shadow-xs",
			"hover:bg-accent",
			"font-medium",
			"border",
			"opacity-50",
		]) {
			expect(isLayoutClass(token), token).toBe(false);
		}
	});
});
