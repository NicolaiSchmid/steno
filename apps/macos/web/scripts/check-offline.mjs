#!/usr/bin/env node
/**
 * Greps dist/ for absolute http(s) URLs (plan Decision 9). The bundle runs
 * under `connect-src 'none'`, so any URL that could be fetched is a bug. The
 * allow-list names strings that are never requested: XML namespaces and the
 * error-decoder prefixes React and Base UI put in their messages.
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";

const ALLOWED = [
	"http://www.w3.org/",
	"https://react.dev/errors/",
	"https://base-ui.com/production-error",
];

function walk(directory, out = []) {
	for (const entry of readdirSync(directory)) {
		const path = join(directory, entry);
		if (statSync(path).isDirectory()) {
			walk(path, out);
		} else {
			out.push(path);
		}
	}
	return out;
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
