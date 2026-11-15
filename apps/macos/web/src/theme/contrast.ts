/**
 * WCAG 2.2 contrast for the tokens in `src/theme.css`. Colours are sRGB
 * with straight alpha; a translucent colour is composited source-over onto
 * the surface it is painted on before it is measured, as the screen shows
 * it. `color-mix(in srgb, …)` mixes premultiplied, as CSS Color 5 says.
 */

export interface Rgba {
	/** 0 to 255, unrounded. */
	r: number;
	g: number;
	b: number;
	/** 0 to 1. */
	a: number;
}

/** Resolves `var(--name)` to the token's declared value. */
export type Resolve = (name: string) => string | undefined;

const NAMED: Record<string, Rgba> = {
	transparent: { r: 0, g: 0, b: 0, a: 0 },
	white: { r: 255, g: 255, b: 255, a: 1 },
	black: { r: 0, g: 0, b: 0, a: 1 },
};

/** Splits on commas at the top level only, so nested `var(…)` stays whole. */
function splitTopLevel(text: string, separator: string): string[] {
	const parts: string[] = [];
	let depth = 0;
	let start = 0;
	for (let index = 0; index < text.length; index += 1) {
		const char = text[index];
		if (char === "(") depth += 1;
		else if (char === ")") depth -= 1;
		else if (char === separator && depth === 0) {
			parts.push(text.slice(start, index).trim());
			start = index + 1;
		}
	}
	parts.push(text.slice(start).trim());
	return parts;
}

function parseHex(hex: string): Rgba {
	const digits = hex.slice(1);
	const expanded =
		digits.length <= 4
			? [...digits].map((digit) => digit + digit).join("")
			: digits;
	if (!/^[0-9a-f]{6}([0-9a-f]{2})?$/i.test(expanded)) {
		throw new Error(`not a hex colour: ${hex}`);
	}
	const channel = (at: number) =>
		Number.parseInt(expanded.slice(at, at + 2), 16);
	return {
		r: channel(0),
		g: channel(2),
		b: channel(4),
		a: expanded.length === 8 ? channel(6) / 255 : 1,
	};
}

function parseNumber(text: string, scale: number): number {
	const value = Number.parseFloat(text);
	if (Number.isNaN(value)) throw new Error(`not a number: ${text}`);
	return text.trim().endsWith("%") ? (value / 100) * scale : value;
}

function parseRgb(inner: string): Rgba {
	// Both `rgb(1, 2, 3, 0.5)` and `rgb(1 2 3 / 50%)`.
	const [channels, slashAlpha] = inner.split("/").map((part) => part.trim());
	const parts = (channels ?? "").includes(",")
		? (channels ?? "").split(",").map((part) => part.trim())
		: (channels ?? "").split(/\s+/);
	const [r, g, b, commaAlpha] = parts;
	if (r === undefined || g === undefined || b === undefined) {
		throw new Error(`not an rgb() colour: ${inner}`);
	}
	const alpha = slashAlpha ?? commaAlpha;
	return {
		r: parseNumber(r, 255),
		g: parseNumber(g, 255),
		b: parseNumber(b, 255),
		a: alpha === undefined ? 1 : parseNumber(alpha, 1),
	};
}

/** `color-mix(in srgb, A p%, B q%)`, premultiplied, with CSS's percentage rules. */
function parseColorMix(inner: string, resolve: Resolve): Rgba {
	const [space, first, second] = splitTopLevel(inner, ",");
	if (space !== "in srgb" || first === undefined || second === undefined) {
		throw new Error(`only color-mix(in srgb, …) is measured: ${inner}`);
	}
	const operand = (text: string) => {
		const match = /^(.*?)(?:\s+([\d.]+)%)?$/.exec(text.trim());
		const percent = match?.[2];
		return {
			colour: parseColor(match?.[1] ?? text, resolve),
			weight: percent === undefined ? undefined : Number(percent) / 100,
		};
	};
	const a = operand(first);
	const b = operand(second);
	let p1 = a.weight ?? (b.weight === undefined ? 0.5 : 1 - b.weight);
	let p2 = b.weight ?? 1 - p1;
	const sum = p1 + p2;
	if (sum <= 0) throw new Error(`color-mix weights sum to zero: ${inner}`);
	// Weights over 100 % scale down; under 100 % they scale the alpha.
	const alphaScale = Math.min(sum, 1);
	p1 /= sum;
	p2 /= sum;
	const alpha = a.colour.a * p1 + b.colour.a * p2;
	const channel = (key: "r" | "g" | "b") =>
		alpha === 0
			? 0
			: (a.colour[key] * a.colour.a * p1 + b.colour[key] * b.colour.a * p2) /
				alpha;
	return {
		r: channel("r"),
		g: channel("g"),
		b: channel("b"),
		a: alpha * alphaScale,
	};
}

/** Hex, `rgb()`/`rgba()`, `color-mix(in srgb, …)`, a few names and `var()`. */
export function parseColor(
	value: string,
	resolve: Resolve = () => undefined,
): Rgba {
	const text = value.trim().toLowerCase();
	const named = NAMED[text];
	if (named) return { ...named };
	if (text.startsWith("#")) return parseHex(text);
	const call = /^([a-z-]+)\((.*)\)$/s.exec(text);
	if (!call) throw new Error(`not a colour: ${value}`);
	const [, fn, inner = ""] = call;
	if (fn === "rgb" || fn === "rgba") return parseRgb(inner);
	if (fn === "color-mix") return parseColorMix(inner, resolve);
	if (fn === "var") {
		const [name = "", fallback] = splitTopLevel(inner, ",");
		const resolved = resolve(name) ?? fallback;
		if (resolved === undefined) throw new Error(`unresolved ${name}`);
		return parseColor(resolved, resolve);
	}
	throw new Error(`not a colour: ${value}`);
}

/** Source-over: `top` painted on `bottom`. The result is as opaque as both together. */
export function composite(top: Rgba, bottom: Rgba): Rgba {
	const a = top.a + bottom.a * (1 - top.a);
	if (a === 0) return { r: 0, g: 0, b: 0, a: 0 };
	const channel = (key: "r" | "g" | "b") =>
		(top[key] * top.a + bottom[key] * bottom.a * (1 - top.a)) / a;
	return { r: channel("r"), g: channel("g"), b: channel("b"), a };
}

/** Paints a stack bottom first: `layers(canvas, tint, hover)`. The bottom must be opaque. */
export function layers(bottom: Rgba, ...above: Rgba[]): Rgba {
	if (bottom.a < 1) throw new Error("the bottom layer must be opaque");
	return above.reduce((under, top) => composite(top, under), bottom);
}

function linear(channel: number): number {
	const c = channel / 255;
	return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
}

/** WCAG relative luminance of an opaque colour. */
export function luminance(colour: Rgba): number {
	return (
		0.2126 * linear(colour.r) +
		0.7152 * linear(colour.g) +
		0.0722 * linear(colour.b)
	);
}

/**
 * The contrast ratio of `foreground` on `background`, unrounded. A
 * translucent foreground is composited onto the background first; the
 * background must be opaque.
 */
export function contrast(foreground: Rgba, background: Rgba): number {
	if (background.a < 1) throw new Error("the background must be opaque");
	const top = composite(foreground, background);
	const l1 = luminance(top);
	const l2 = luminance(background);
	return (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05);
}

/** `#rrggbb` of an opaque colour, each channel rounded. */
export function toHex(colour: Rgba): string {
	const channel = (value: number) =>
		Math.round(Math.min(255, Math.max(0, value)))
			.toString(16)
			.padStart(2, "0");
	return `#${channel(colour.r)}${channel(colour.g)}${channel(colour.b)}`;
}
