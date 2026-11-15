#!/usr/bin/env node
/**
 * `pnpm lint:ui`: callers of `src/components/ui` may only add layout classes
 * through `className`. Everything about a component's look (colour, type,
 * radius, shadow, padding) belongs in the component itself, behind `variant`
 * and `size`.
 *
 * And nothing anywhere, the kit included, paints a colour the contrast test
 * (src/theme/contrast.test.ts) cannot see: no `/NN` on a colour token (name
 * the veil in src/theme.css instead), and no `opacity-*` on what holds text
 * or icons, except in a disabled state, on `aria-hidden` decoration, and the
 * 0 and 100 a transition starts or ends at. Text is sized from the type
 * scale in src/theme.css only: Tailwind's own sizes and arbitrary sizes are
 * rejected everywhere.
 *
 * Usage: node scripts/check-ui-restyle.mjs [--root src] [--ui src/components/ui] [--theme src/theme.css]
 * Exit 1 lists every offending class as `file:line: <Component> "class"`.
 */
import { readdirSync, readFileSync } from "node:fs";
import { join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";

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
];

function parseArgs(argv) {
	const options = {
		root: "src",
		ui: "src/components/ui",
		theme: "src/theme.css",
	};
	for (let index = 0; index < argv.length; index += 1) {
		const argument = argv[index];
		if (argument === "--root") {
			options.root = argv[index + 1] ?? options.root;
			index += 1;
		} else if (argument === "--ui") {
			options.ui = argv[index + 1] ?? options.ui;
			index += 1;
		} else if (argument === "--theme") {
			options.theme = argv[index + 1] ?? options.theme;
			index += 1;
		}
	}
	return options;
}

function walk(directory, predicate) {
	return readdirSync(directory, { recursive: true, withFileTypes: true })
		.filter((entry) => entry.isFile())
		.map((entry) => join(entry.parentPath, entry.name))
		.filter(
			(path) => !path.split("/").includes("node_modules") && predicate(path),
		);
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

/** The colour tokens Tailwind knows: every `--color-<name>` in the theme. */
export function collectColourTokens(themeSource) {
	return new Set(
		[...themeSource.matchAll(/--color-([a-z0-9-]+)\s*:/g)]
			.map((match) => match[1])
			.filter((name) => name !== "*"),
	);
}

/** Tailwind's default sizes and arbitrary lengths: not on the type scale. */
const OFF_SCALE =
	/^text-(?:[2-9]?xs|sm|base|lg|[2-9]?xl|\[\d[\d.]*(?:px|rem|em)\])$/;

/** The variant chain in front of a class (`hover:data-disabled:`). */
function variantsOf(token) {
	const base = baseClass(token);
	return token.slice(0, token.length - base.length);
}

/**
 * Why a class paints a colour the contrast test cannot see, or sizes text
 * off the scale, or null: a colour token with an opacity modifier, an
 * element opacity between 0 and 100 outside a disabled state, a Tailwind
 * or arbitrary text size.
 */
export function paintProblem(token, colours, hidden = false) {
	const base = baseClass(token).replace(/!$/, "");
	const slash = base.lastIndexOf("/");
	if (slash > 0 && !base.slice(0, slash).includes("[")) {
		const prefix = base.slice(0, slash);
		for (const colour of colours) {
			if (prefix.endsWith(`-${colour}`)) {
				return `veils --${colour}; name the veil in src/theme.css`;
			}
		}
	}
	if (OFF_SCALE.test(base)) {
		return "sizes text off the type scale; use text-display, text-headline, text-reading, text-title, text-body, text-label, text-meta or text-caption";
	}
	const opacity = /^opacity-(.+)$/.exec(base);
	if (opacity && opacity[1] !== "0" && opacity[1] !== "100" && !hidden) {
		if (!variantsOf(token).includes("disabled")) {
			return "fades text or icons below their measured contrast";
		}
	}
	return null;
}

function hasAriaHidden(element) {
	return element.attributes.properties.some(
		(attribute) =>
			ts.isJsxAttribute(attribute) &&
			attribute.name.getText() === "aria-hidden" &&
			attribute.initializer?.getText() !== "{false}",
	);
}

/** Every class-like token in the file's string literals that paints unseen. */
export function checkPaint(source, file, colours) {
	const sourceFile = ts.createSourceFile(
		file,
		source,
		ts.ScriptTarget.Latest,
		true,
		file.endsWith(".tsx") ? ts.ScriptKind.TSX : ts.ScriptKind.TS,
	);
	const findings = [];
	const report = (text, node, hidden) => {
		for (const token of text.split(/\s+/).filter(Boolean)) {
			const problem = paintProblem(token, colours, hidden);
			if (problem) {
				const { line } = sourceFile.getLineAndCharacterOfPosition(
					node.getStart(sourceFile),
				);
				findings.push({ file, line: line + 1, token, problem });
			}
		}
	};
	const visit = (node, hidden) => {
		let inside = hidden;
		if (ts.isJsxElement(node) && hasAriaHidden(node.openingElement)) {
			inside = true;
		} else if (ts.isJsxSelfClosingElement(node) && hasAriaHidden(node)) {
			inside = true;
		}
		if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) {
			report(node.text, node, inside);
		} else if (ts.isTemplateExpression(node)) {
			report(node.head.text, node, inside);
			for (const span of node.templateSpans) {
				report(span.literal.text, node, inside);
			}
		}
		ts.forEachChild(node, (child) => visit(child, inside));
	};
	visit(sourceFile, false);
	return findings;
}

export function run({
	root,
	ui,
	theme = "src/theme.css",
	cwd = process.cwd(),
}) {
	const rootDirectory = resolve(cwd, root);
	const uiDirectory = resolve(cwd, ui);
	const uiNames = collectUiExports(uiDirectory);
	const colours = collectColourTokens(
		readFileSync(resolve(cwd, theme), "utf8"),
	);
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
	// The paint rule covers the kit too, and plain modules that build classes.
	const painted = walk(
		rootDirectory,
		(path) => /\.tsx?$/.test(path) && !/\.test\.tsx?$/.test(path),
	);
	const paint = [];
	for (const file of painted) {
		paint.push(
			...checkPaint(readFileSync(file, "utf8"), relative(cwd, file), colours),
		);
	}
	return { files: files.length, uiNames, findings, paint };
}

const invokedDirectly =
	process.argv[1] &&
	resolve(process.argv[1]) === fileURLToPath(import.meta.url);

if (invokedDirectly) {
	const options = parseArgs(process.argv.slice(2));
	const result = run(options);
	for (const finding of result.findings) {
		console.error(
			`${finding.file}:${finding.line}: <${finding.name}> className "${finding.token}" restyles a ui component; pick a variant or move the look into src/components/ui`,
		);
	}
	for (const finding of result.paint) {
		console.error(
			`${finding.file}:${finding.line}: "${finding.token}" ${finding.problem}`,
		);
	}
	if (result.findings.length > 0 || result.paint.length > 0) {
		console.error(
			`\n${result.findings.length} restyle(s) in ${result.files} file(s); callers may add layout classes only. ${result.paint.length} unmeasured paint(s).`,
		);
		process.exit(1);
	}
	console.log(
		`lint:ui ok: ${result.files} file(s) checked against ${result.uiNames.size} ui exports, no unmeasured paint`,
	);
}
