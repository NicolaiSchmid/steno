#!/usr/bin/env node
/**
 * Fails when the production bundle carries the mock bridge or a recorded
 * fixture (plan `2026-09-29-macos-webview-ui.md`, the WP2 deviation closed
 * in WP5). `vite build` resolves `#bridge-fallback` to `fallback-none.ts`,
 * so neither should be reachable; this is the gate that proves it. Three
 * signals: a chunk named after a fixture key (`meetings.list-<hash>.js`),
 * a string value a fixture carries and the page's own sources never spell
 * (the longest such one per file; copy the page shares with a fixture is
 * no evidence), and the literals only `src/bridge/mock-transport.ts`
 * carries. The screens bundle (`vite build --mode screens`, `dist-screens/`)
 * is meant to carry them and is never checked here.
 */
import { readdirSync, readFileSync } from "node:fs";
import { basename, join, relative } from "node:path";

const FIXTURES = "fixtures/bridge";
/** Strings the mock transport alone spells; minification keeps literals. */
const MOCK_MARKERS = ["onboarding-vault-saved", "summaries-connected"];
/** A fixture string shorter than this is too common to be a marker. */
const MIN_MARKER_LENGTH = 16;
/** Sources that may spell fixture strings without being fixtures. */
const PAGE_SOURCE_EXCLUDED = [
	"src/bridge/mock-transport.ts",
	"src/bridge/fallback-mock.ts",
];

function walk(directory) {
	return readdirSync(directory, { recursive: true, withFileTypes: true })
		.filter((entry) => entry.isFile())
		.map((entry) => join(entry.parentPath, entry.name));
}

/** Every string value in a JSON document, however deep. */
function strings(value, out = []) {
	if (typeof value === "string") {
		out.push(value);
	} else if (Array.isArray(value)) {
		for (const item of value) strings(item, out);
	} else if (value && typeof value === "object") {
		for (const item of Object.values(value)) strings(item, out);
	}
	return out;
}

/** The page's own sources, one text: what it spells is not a fixture trace. */
function pageSources() {
	return walk("src")
		.filter(
			(file) =>
				/\.tsx?$/.test(file) &&
				!/\.test\.tsx?$/.test(file) &&
				!file.startsWith(join("src", "test")) &&
				!PAGE_SOURCE_EXCLUDED.includes(file),
		)
		.map((file) => readFileSync(file, "utf8"))
		.join("\n");
}

/**
 * One marker per fixture file: its longest string value the page never
 * spells itself, when one is long enough. A file without one is still
 * covered by its chunk name.
 */
function fixtureMarkers(directory, known) {
	const markers = new Map();
	for (const file of walk(directory)) {
		if (!file.endsWith(".json") || basename(file) === "index.json") continue;
		const marker = strings(JSON.parse(readFileSync(file, "utf8")))
			.filter((text) => text.length >= MIN_MARKER_LENGTH)
			.sort((a, b) => b.length - a.length)
			.find((text) => !known.includes(text));
		if (marker) markers.set(basename(file, ".json"), marker);
	}
	return markers;
}

/** The chunk names Vite gives the fixtures: `<key>-<hash>.js`. */
function fixtureChunkPattern(directory) {
	const keys = JSON.parse(readFileSync(join(directory, "index.json"), "utf8"));
	const escaped = keys.map((key) => key.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
	return new RegExp(`^(${escaped.join("|")})-[\\w-]+\\.js$`);
}

function findings(root, markers, chunkPattern) {
	const found = [];
	for (const file of walk(root)) {
		const name = relative(process.cwd(), file);
		if (chunkPattern.test(basename(file))) {
			found.push(`${name}: a fixture chunk`);
			continue;
		}
		const text = readFileSync(file, "utf8");
		for (const marker of MOCK_MARKERS) {
			if (text.includes(marker))
				found.push(`${name}: mock transport (${marker})`);
		}
		for (const [key, marker] of markers) {
			if (text.includes(marker)) found.push(`${name}: fixture ${key}`);
		}
	}
	return found;
}

const root = process.argv[2] ?? "dist";
const found = findings(
	root,
	fixtureMarkers(FIXTURES, pageSources()),
	fixtureChunkPattern(FIXTURES),
);
if (found.length > 0) {
	for (const finding of found) {
		console.error(finding);
	}
	console.error(
		`\n${found.length} mock or fixture trace(s) in ${root}. The app bundle must not carry the mock bridge; see vite.config.ts.`,
	);
	process.exit(1);
}
console.log(`check:bundle ok: no mock transport or fixture in ${root}`);
