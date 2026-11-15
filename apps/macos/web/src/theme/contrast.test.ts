// @vitest-environment node
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import {
	composite,
	contrast,
	layers,
	parseColor,
	type Rgba,
	toHex,
} from "./contrast";
import { dark, light, type Palette } from "./palette";

describe("contrast", () => {
	const white = parseColor("#ffffff");
	const ratio = (foreground: string, background = "#ffffff") =>
		contrast(parseColor(foreground), parseColor(background));

	// Published pairs: WebAIM's checker and the WCAG understanding documents.
	it.each([
		["#000000", "#ffffff", 21],
		["#ffffff", "#ffffff", 1],
		["#767676", "#ffffff", 4.54],
		["#777777", "#ffffff", 4.48],
		["#595959", "#ffffff", 7],
		["#0000ff", "#ffffff", 8.59],
		["#ff0000", "#ffffff", 4],
		["#00ff00", "#000000", 15.3],
	])("%s on %s is %d", (foreground, background, expected) => {
		expect(ratio(foreground, background)).toBeCloseTo(expected, 1);
	});

	it("reads short hex, rgb() and rgba() in both syntaxes", () => {
		expect(parseColor("#fff")).toEqual(white);
		expect(parseColor("rgb(255, 255, 255)")).toEqual(white);
		expect(parseColor("rgba(0, 0, 0, 0.5)").a).toBe(0.5);
		expect(parseColor("rgb(0 0 0 / 50%)").a).toBe(0.5);
		expect(parseColor("#00000080").a).toBeCloseTo(0.5, 2);
	});

	it("composites source-over", () => {
		const half = composite(parseColor("rgba(0, 0, 0, 0.5)"), white);
		expect(half.r).toBeCloseTo(127.5);
		expect(half.a).toBe(1);
		expect(toHex(layers(white, parseColor("rgba(0, 0, 0, 0.5)")))).toBe(
			"#808080",
		);
	});

	it("mixes premultiplied in srgb, as CSS does", () => {
		expect(toHex(parseColor("color-mix(in srgb, #000000 50%, #ffffff)"))).toBe(
			"#808080",
		);
		// With transparent the colour keeps its channels and takes the weight as alpha.
		const veil = parseColor("color-mix(in srgb, #ff0000 32%, transparent)");
		expect(veil.r).toBeCloseTo(255);
		expect(veil.a).toBeCloseTo(0.32);
		// Weights under 100 % scale the alpha.
		expect(
			parseColor("color-mix(in srgb, #ff0000 20%, #0000ff 30%)").a,
		).toBeCloseTo(0.5);
	});

	it("resolves var() through the given tokens", () => {
		const tokens: Record<string, string> = {
			"--a": "#000000",
			"--b": "color-mix(in srgb, var(--a) 50%, var(--c, #ffffff))",
		};
		expect(toHex(parseColor("var(--b)", (name) => tokens[name]))).toBe(
			"#808080",
		);
	});

	it("measures a translucent foreground on what it is painted on", () => {
		expect(ratio("rgba(0, 0, 0, 0.5)")).toBeCloseTo(
			ratio("rgb(127.5 127.5 127.5)"),
			5,
		);
	});
});

/* ---- The stylesheet ---------------------------------------------------- */

const css = readFileSync(
	new URL("../theme.css", import.meta.url),
	"utf8",
).replace(/\/\*[\s\S]*?\*\//g, "");

/** The `--name: value` declarations of the top-level block `selector { … }`. */
function declarations(selector: string): Map<string, string> {
	const start = css.indexOf(`\n${selector} {`);
	if (start < 0) throw new Error(`no ${selector} block in theme.css`);
	const open = css.indexOf("{", start);
	const close = css.indexOf("}", open);
	const body = css.slice(open + 1, close);
	const map = new Map<string, string>();
	for (const match of body.matchAll(/(--[a-z0-9-]+)\s*:\s*([^;]+);/g)) {
		const [, name = "", value = ""] = match;
		map.set(name, value.replace(/\s+/g, " ").trim());
	}
	return map;
}

type Mode = "light" | "dark";
type SurfaceName = "sidebar" | "canvas" | "card" | "popover";

/** The aliases, declared on `:root` and again on every `.dark` element. */
const ALIASES = declarations(":where(:root, .dark)");

const MODES: Record<Mode, Map<string, string>> = {
	light: new Map([...declarations(":root"), ...ALIASES]),
	dark: new Map([
		...declarations(":root"),
		...ALIASES,
		...declarations(".dark"),
	]),
};

/** What each surface utility points the scoped tokens at. */
const SURFACES: Record<SurfaceName, Map<string, string>> = {
	sidebar: declarations("@utility surface-sidebar"),
	canvas: declarations("@utility surface-canvas"),
	card: declarations("@utility surface-card"),
	popover: declarations("@utility surface-popover"),
};

/**
 * A token as painted inside `surface` in `mode`. The surface's own scoped
 * tokens win; everything else is the mode's, as on the page.
 */
function token(mode: Mode, name: string, surface?: SurfaceName): Rgba {
	const scoped = surface ? SURFACES[surface] : undefined;
	const lookup = (key: string) => scoped?.get(key) ?? MODES[mode].get(key);
	return parseColor(`var(${name})`, lookup);
}

interface Host {
	name: string;
	colour: Rgba;
}

/** The surface itself and every row fill drawn on it. */
function hosts(mode: Mode, surface: SurfaceName): Host[] {
	const base = token(mode, "--surface", surface);
	const fill = (name: string) => layers(base, token(mode, name, surface));
	return [
		{ name: surface, colour: base },
		{ name: `${surface} accent`, colour: fill("--accent") },
		{ name: `${surface} row hover`, colour: fill("--row-hover") },
		{ name: `${surface} row selected`, colour: fill("--row-selected") },
	];
}

const ALL: SurfaceName[] = ["sidebar", "canvas", "card", "popover"];

function everyHost(mode: Mode, surfaces = ALL): Host[] {
	return surfaces.flatMap((surface) => hosts(mode, surface));
}

/** Each tint composited over every host it may appear on. */
function tinted(mode: Mode, tint: string, surfaces = ALL): Host[] {
	return everyHost(mode, surfaces).map((host) => ({
		name: `${tint} over ${host.name}`,
		colour: layers(host.colour, token(mode, tint)),
	}));
}

interface Check {
	name: string;
	ratio: number;
	minimum: number;
}

const TEXT = 4.5;
const NON_TEXT = 3;
const STEP = 1.07;

const TONES = ["warning", "success", "info", "destructive"] as const;
const PEOPLE = ["--p1", "--p2", "--p3", "--p4"] as const;

function checks(mode: Mode): Check[] {
	const list: Check[] = [];
	const on = (foreground: string, where: Host[], minimum: number) => {
		for (const host of where) {
			list.push({
				name: `${mode}: ${foreground} on ${host.name}`,
				ratio: contrast(token(mode, foreground), host.colour),
				minimum,
			});
		}
	};
	const sidebar = everyHost(mode, ["sidebar"]);
	// Text on every surface it may sit on.
	for (const name of ["--foreground", "--muted-foreground"]) {
		on(name, everyHost(mode), TEXT);
	}
	on("--sidebar-muted-foreground", sidebar, TEXT);
	// Status and live: text and icons bare, and on their own tint over every host.
	for (const tone of TONES) {
		const where = [
			...everyHost(mode, ["sidebar", "canvas", "card"]),
			...tinted(mode, `--${tone}-surface`, ["sidebar", "canvas", "card"]),
		];
		on(`--${tone}-foreground`, where, TEXT);
		on(`--${tone}`, where, NON_TEXT);
	}
	// The warning-outline button's hover.
	on(
		"--warning-foreground",
		tinted(mode, "--warning-surface-hover", ["sidebar", "canvas", "card"]),
		TEXT,
	);
	on("--live-foreground", everyHost(mode, ["sidebar", "canvas", "card"]), TEXT);
	on(
		"--live",
		everyHost(mode, ["sidebar", "canvas", "card", "popover"]),
		NON_TEXT,
	);

	// People: each colour as the initial on its own tint.
	for (const person of PEOPLE) {
		on(person, tinted(mode, `${person}-surface`), TEXT);
	}

	// Fills that carry text.
	const canvas = token(mode, "--background");
	const fills: [string, string][] = [
		["--primary-foreground", "--primary"],
		["--primary-foreground", "--primary-hover"],
		["--on-destructive", "--destructive"],
		["--on-destructive", "--destructive-hover"],
	];
	for (const [foreground, fill] of fills) {
		on(
			foreground,
			[{ name: fill, colour: layers(canvas, token(mode, fill)) }],
			TEXT,
		);
	}

	// The disabled primary: muted words on its fill over each surface, and
	// the fill a step off the canvas so it still reads as a button.
	on(
		"--primary-disabled-foreground",
		(["sidebar", "canvas", "card", "popover"] as const).map((surface) => ({
			name: `--primary-disabled over ${surface}`,
			colour: layers(
				token(mode, "--surface", surface),
				token(mode, "--primary-disabled", surface),
			),
		})),
		TEXT,
	);

	// Non-text: outlines and the switch's off track, the dashed button's
	// outline and its hover, focus, sidebar icons.
	on("--outline", everyHost(mode), NON_TEXT);
	on("--dashed-outline", everyHost(mode), NON_TEXT);
	on("--dashed-outline-hover", everyHost(mode), NON_TEXT);
	on("--focus", everyHost(mode), NON_TEXT);
	on("--sidebar-icon", sidebar, NON_TEXT);
	const primary = layers(canvas, token(mode, "--primary"));
	on(
		"--primary-part-focus",
		[{ name: "--primary", colour: primary }],
		NON_TEXT,
	);

	// The live mark on a primary fill (the sidebar's Stop), resting and
	// hovered, on each surface a primary button sits on.
	on(
		"--live-on-primary",
		(["sidebar", "canvas", "card"] as const).flatMap((surface) =>
			["--primary", "--primary-hover"].map((fill) => ({
				name: `${fill} over ${surface}`,
				colour: layers(
					token(mode, "--surface", surface),
					token(mode, fill, surface),
				),
			})),
		),
		NON_TEXT,
	);

	// The underline tab's indicator on the canvas and a card.
	on("--tab-indicator", everyHost(mode, ["canvas", "card"]), NON_TEXT);

	// Steps: the sidebar to the canvas, the canvas to each raised surface.
	const step = (name: string, a: Rgba, b: Rgba) =>
		list.push({
			name: `${mode}: step ${name}`,
			ratio: contrast(a, b),
			minimum: STEP,
		});
	step("sidebar to canvas", token(mode, "--sidebar"), canvas);
	step(
		"canvas to --primary-disabled",
		canvas,
		layers(canvas, token(mode, "--primary-disabled")),
	);
	for (const raised of ["--card", "--popover"]) {
		if (mode === "light") {
			// Exempt: the light card and popover are the canvas white in a hairline box.
			continue;
		}
		step(`canvas to ${raised}`, canvas, token(mode, raised));
	}
	return list;
}

describe.each([
	["light", light],
	["dark", dark],
] as const)("theme.css in %s", (mode, palette: Palette) => {
	it("holds the computed palette", () => {
		const drift = Object.entries(palette)
			.map(([name, value]) => [name, toHex(token(mode, name)), value])
			.filter(([, actual, expected]) => actual !== expected?.toLowerCase())
			.map(([name, actual, expected]) => `${name}: ${actual}, not ${expected}`);
		expect(drift).toEqual([]);
	});

	it("passes every pair it measures", () => {
		const list = checks(mode);
		expect(list.length).toBeGreaterThan(100);
		const failing = list
			.filter((check) => check.ratio < check.minimum)
			.map(
				(check) =>
					`${check.name}: ${check.ratio.toFixed(2)} < ${check.minimum}`,
			);
		expect(failing).toEqual([]);
	});
});
