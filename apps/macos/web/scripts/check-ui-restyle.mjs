#!/usr/bin/env node
/**
 * `pnpm lint:ui`: callers of `src/components/ui` may only add layout classes
 * through `className`. Everything about a component's look (colour, type,
 * radius, shadow, padding) belongs in the component itself, behind `variant`
 * and `size`. Plan .plans/2026-09-29-macos-webview-ui.md, Decision 4.
 *
 * Usage: node scripts/check-ui-restyle.mjs [--root src] [--ui src/components/ui]
 * Exit 1 lists every offending class as `file:line: <Component> "class"`.
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, resolve } from "node:path";

const LAYOUT_TOKENS = [
	"w-",
	"h-",
	"min-",
	"max-",
	"size-",
	"flex",
	"grid",
	"col-",
	"row-",
	"gap-",
	"m-",
	"mx-",
	"my-",
	"mt-",
	"mr-",
	"mb-",
	"ml-",
	"self-",
	"justify-",
	"items-",
	"order-",
	"shrink",
	"grow",
	"basis-",
	"absolute",
	"relative",
	"fixed",
	"sticky",
	"inset-",
	"top-",
	"left-",
	"right-",
	"bottom-",
	"z-",
	"hidden",
	"block",
	"inline",
	"overflow-",
	"truncate",
	"shrink-0",
];

function parseArgs(argv) {
	const options = { root: "src", ui: "src/components/ui" };
	for (let index = 0; index < argv.length; index += 1) {
		const argument = argv[index];
		if (argument === "--root") {
			options.root = argv[index + 1] ?? options.root;
			index += 1;
		} else if (argument === "--ui") {
			options.ui = argv[index + 1] ?? options.ui;
			index += 1;
		}
	}
	return options;
}

function walk(directory, predicate, out = []) {
	for (const entry of readdirSync(directory)) {
		const path = join(directory, entry);
		if (entry === "node_modules") {
			continue;
		}
		if (statSync(path).isDirectory()) {
			walk(path, predicate, out);
		} else if (predicate(path)) {
			out.push(path);
		}
	}
	return out;
}

/** Every PascalCase export from the ui folder. */
export function collectUiExports(uiDirectory) {
	const names = new Set();
	const files = walk(uiDirectory, (path) => /\.tsx?$/.test(path));
	const pattern = /export\s+(?:function|const|class)\s+([A-Z][A-Za-z0-9]*)/g;
	for (const file of files) {
		const source = readFileSync(file, "utf8");
		for (const match of source.matchAll(pattern)) {
			names.add(match[1]);
		}
	}
	return names;
}

/** Strips variant prefixes (`hover:`, `md:`, `data-[x]:`) and `!`/`-` marks. */
export function baseClass(token) {
	let base = token;
	let depth = 0;
	let lastColon = -1;
	for (let index = 0; index < base.length; index += 1) {
		const char = base[index];
		if (char === "[" || char === "(") {
			depth += 1;
		} else if (char === "]" || char === ")") {
			depth -= 1;
		} else if (char === ":" && depth === 0) {
			lastColon = index;
		}
	}
	if (lastColon >= 0) {
		base = base.slice(lastColon + 1);
	}
	if (base.startsWith("!")) {
		base = base.slice(1);
	}
	if (base.startsWith("-")) {
		base = base.slice(1);
	}
	return base;
}

export function isLayoutClass(token) {
	const base = baseClass(token);
	if (base === "") {
		return true;
	}
	return LAYOUT_TOKENS.some((allowed) => {
		if (allowed.endsWith("-")) {
			return base.startsWith(allowed);
		}
		return base === allowed || base.startsWith(`${allowed}-`);
	});
}

/**
 * Finds the end of a JSX opening tag starting at `start` (the `<`), skipping
 * braces and strings. Returns the index of the closing `>`, or -1.
 */
function findTagEnd(source, start) {
	let depth = 0;
	let quote = null;
	for (let index = start + 1; index < source.length; index += 1) {
		const char = source[index];
		if (quote) {
			if (char === "\\") {
				index += 1;
			} else if (char === quote) {
				quote = null;
			}
			continue;
		}
		if (char === '"' || char === "'" || char === "`") {
			quote = char;
		} else if (char === "{") {
			depth += 1;
		} else if (char === "}") {
			depth -= 1;
		} else if (char === ">" && depth === 0) {
			return index;
		}
	}
	return -1;
}

/** The literal class strings inside a `className=` attribute value. */
function classLiterals(attributeText) {
	const literals = [];
	const pattern =
		/"([^"\\]*(?:\\.[^"\\]*)*)"|'([^'\\]*(?:\\.[^'\\]*)*)'|`([^`]*)`/g;
	for (const match of attributeText.matchAll(pattern)) {
		const literal = match[1] ?? match[2] ?? match[3] ?? "";
		if (match[3] !== undefined) {
			literals.push(literal.replace(/\$\{[^}]*\}/g, " "));
		} else {
			literals.push(literal);
		}
	}
	return literals;
}

/** Extracts the value expression of `className=` in a tag's attribute text. */
function classNameValue(attributes) {
	const at = attributes.search(/(^|\s)className\s*=/);
	if (at < 0) {
		return null;
	}
	const equals = attributes.indexOf("=", at);
	let index = equals + 1;
	while (index < attributes.length && /\s/.test(attributes[index])) {
		index += 1;
	}
	const open = attributes[index];
	if (open === '"' || open === "'") {
		const close = attributes.indexOf(open, index + 1);
		return attributes.slice(index, close < 0 ? undefined : close + 1);
	}
	if (open === "{") {
		let depth = 0;
		for (let cursor = index; cursor < attributes.length; cursor += 1) {
			if (attributes[cursor] === "{") {
				depth += 1;
			} else if (attributes[cursor] === "}") {
				depth -= 1;
				if (depth === 0) {
					return attributes.slice(index + 1, cursor);
				}
			}
		}
	}
	return null;
}

export function checkSource(source, file, uiNames) {
	const findings = [];
	const tagPattern = /<([A-Z][A-Za-z0-9]*)(?=[\s/>])/g;
	for (const match of source.matchAll(tagPattern)) {
		const name = match[1];
		if (!uiNames.has(name)) {
			continue;
		}
		const start = match.index;
		const end = findTagEnd(source, start);
		if (end < 0) {
			continue;
		}
		const attributes = source.slice(start + 1 + name.length, end);
		const value = classNameValue(attributes);
		if (value === null) {
			continue;
		}
		const line = source.slice(0, start).split("\n").length;
		for (const literal of classLiterals(value)) {
			for (const token of literal.split(/\s+/).filter(Boolean)) {
				if (!isLayoutClass(token)) {
					findings.push({ file, line, name, token });
				}
			}
		}
	}
	return findings;
}

export function run({ root, ui, cwd = process.cwd() }) {
	const rootDirectory = resolve(cwd, root);
	const uiDirectory = resolve(cwd, ui);
	const uiNames = collectUiExports(uiDirectory);
	const files = walk(
		rootDirectory,
		(path) => path.endsWith(".tsx") && !path.startsWith(`${uiDirectory}/`),
	);
	const findings = [];
	for (const file of files) {
		findings.push(
			...checkSource(readFileSync(file, "utf8"), relative(cwd, file), uiNames),
		);
	}
	return { files: files.length, uiNames, findings };
}

const invokedDirectly =
	process.argv[1] &&
	resolve(process.argv[1]) === new URL(import.meta.url).pathname;

if (invokedDirectly) {
	const options = parseArgs(process.argv.slice(2));
	const result = run(options);
	if (result.findings.length > 0) {
		for (const finding of result.findings) {
			console.error(
				`${finding.file}:${finding.line}: <${finding.name}> className "${finding.token}" restyles a ui component; pick a variant or move the look into src/components/ui`,
			);
		}
		console.error(
			`\n${result.findings.length} restyle(s) in ${result.files} file(s). Callers may add layout classes only.`,
		);
		process.exit(1);
	}
	console.log(
		`lint:ui ok: ${result.files} file(s) checked against ${result.uiNames.size} ui exports`,
	);
}
