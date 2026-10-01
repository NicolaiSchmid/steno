#!/usr/bin/env node
/**
 * Greps dist/ for absolute http(s) URLs (plan Decision 9). The bundle runs
 * under `connect-src 'none'`, so any URL that could be fetched is a bug. The
 * allow-list names strings that are never requested: XML namespaces, the
 * error-decoder prefixes React and Base UI put in their messages, and the
 * JSON Schema identifiers and IPv6 probe inside zod. The recorded fixtures
 * (one carries a local server address) never reach `dist/`: the production
 * build resolves `#bridge-fallback` without the mock bridge, and
 * `check-bundle.mjs` fails when a fixture lands there anyway.
 */
import { readdirSync, readFileSync } from "node:fs";
import { join, relative } from "node:path";

const ALLOWED = [
	"http://www.w3.org/",
	"https://react.dev/errors/",
	"https://base-ui.com/production-error",
	"https://json-schema.org/",
	"http://json-schema.org/",
	// zod validates an IPv6 host by building `http://[${host}]` for `new URL`.
	"http://[",
];

function walk(directory) {
	return readdirSync(directory, { recursive: true, withFileTypes: true })
		.filter((entry) => entry.isFile())
		.map((entry) => join(entry.parentPath, entry.name));
}

const root = process.argv[2] ?? "dist";
const pattern = /https?:\/\/[^\s"'`)]+/g;
const findings = [];
for (const file of walk(root)) {
	const text = readFileSync(file, "utf8");
	for (const match of text.matchAll(pattern)) {
		const url = match[0];
		if (!ALLOWED.some((prefix) => url.startsWith(prefix))) {
			findings.push(`${relative(process.cwd(), file)}: ${url}`);
		}
	}
}

if (findings.length > 0) {
	for (const finding of findings) {
		console.error(finding);
	}
	console.error(
		`\n${findings.length} absolute URL(s) in ${root}. The bundle must be offline.`,
	);
	process.exit(1);
}
console.log(`check:offline ok: no fetchable URL in ${root}`);
