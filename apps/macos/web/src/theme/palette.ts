/**
 * The computed palette: the colour tokens in `src/theme.css` that the
 * contrast test (`contrast.test.ts`) measures, and where each comes from.
 * The test fails when the stylesheet drifts from these values.
 *
 * Dark is not an inversion. Its surfaces are steps from a #121212 base
 * toward white, achromatic as the primary is: the sidebar is the base, the
 * canvas lifts by 3.5 percent of the way, cards and popovers lift further,
 * and row fills lift again, so elevation reads as a lighter surface where
 * a shadow would not show. Status, live and people colours take their own
 * light Tailwind steps on dark rather than the light mode's.
 */
import { parseColor, toHex } from "./contrast";

/** Tailwind's colours at the steps Steno uses. */
const tailwind = {
	amber: { 300: "#fcd34d", 800: "#92400e" },
	emerald: { 300: "#6ee7b7", 800: "#065f46" },
	blue: { 300: "#93c5fd", 700: "#1d4ed8" },
	red: { 300: "#fca5a5", 400: "#f87171", 600: "#dc2626", 700: "#b91c1c" },
	teal: { 300: "#5eead4", 800: "#115e59" },
	violet: { 300: "#c4b5fd", 700: "#6d28d9" },
	green: { 800: "#166534" },
	lime: { 300: "#bef264" },
} as const;

/** The dark base: the sidebar, and the ground every dark surface lifts from. */
export const DARK_BASE = "#121212";

/** A dark surface `share` of the way from the base to white. */
export function lift(share: number): string {
	const base = parseColor(DARK_BASE);
	const toward = (channel: number) => channel + (255 - channel) * share;
	return toHex({
		r: toward(base.r),
		g: toward(base.g),
		b: toward(base.b),
		a: 1,
	});
}

/** How far each dark step lifts from the base. */
const DARK_STEPS = {
	canvas: 0.035,
	raised: 0.07,
	accent: 0.1,
	hairline: 0.12,
	selected: 0.13,
	outline: 0.44,
	muted: 0.61,
} as const;

export type Palette = Readonly<Record<string, string>>;

/** Light: white canvas and cards, the sidebar and row fills one grey below. */
export const light: Palette = {
	"--sidebar": "#f5f5f5",
	"--background": "#ffffff",
	"--card": "#ffffff",
	"--popover": "#ffffff",
	"--sidebar-accent": "#ededed",
	"--sidebar-row-hover": "#ededed",
	"--sidebar-row-selected": "#ffffff",
	"--canvas-accent": "#f5f5f5",
	"--canvas-row-hover": "#f5f5f5",
	"--canvas-row-selected": "#ededed",
	"--border": "#eaeaeb",
	"--outline": "#858585",
	"--focus": "#68686e",
	"--muted-foreground": "#68686e",
	"--sidebar-muted-foreground": "#68686e",
	"--sidebar-icon": "#68686e",
	"--warning": tailwind.amber[800],
	"--warning-foreground": tailwind.amber[800],
	"--success": tailwind.emerald[800],
	"--success-foreground": tailwind.emerald[800],
	"--info": tailwind.blue[700],
	"--info-foreground": tailwind.blue[700],
	"--destructive-foreground": tailwind.red[700],
	"--live-foreground": tailwind.red[700],
	"--destructive": tailwind.red[600],
	"--live": tailwind.red[600],
	"--on-destructive": "#ffffff",
	"--p1": tailwind.blue[700],
	"--p2": tailwind.teal[800],
	"--p3": tailwind.violet[700],
	"--p4": tailwind.green[800],
};

/** Dark: the ladder from #121212, the Tailwind 300s for words and icons. */
export const dark: Palette = {
	"--sidebar": DARK_BASE,
	"--background": lift(DARK_STEPS.canvas),
	"--card": lift(DARK_STEPS.raised),
	"--popover": lift(DARK_STEPS.raised),
	"--sidebar-accent": lift(DARK_STEPS.accent),
	"--sidebar-row-hover": lift(DARK_STEPS.accent),
	"--sidebar-row-selected": lift(DARK_STEPS.selected),
	"--canvas-accent": lift(DARK_STEPS.accent),
	"--canvas-row-hover": lift(DARK_STEPS.accent),
	"--canvas-row-selected": lift(DARK_STEPS.selected),
	"--border": lift(DARK_STEPS.hairline),
	"--outline": lift(DARK_STEPS.outline),
	"--focus": lift(DARK_STEPS.muted),
	"--muted-foreground": lift(DARK_STEPS.muted),
	"--sidebar-muted-foreground": lift(DARK_STEPS.muted),
	"--sidebar-icon": lift(DARK_STEPS.muted),
	"--warning": tailwind.amber[300],
	"--warning-foreground": tailwind.amber[300],
	"--success": tailwind.emerald[300],
	"--success-foreground": tailwind.emerald[300],
	"--info": tailwind.blue[300],
	"--info-foreground": tailwind.blue[300],
	"--destructive-foreground": tailwind.red[300],
	"--live-foreground": tailwind.red[300],
	"--destructive": tailwind.red[400],
	"--live": tailwind.red[400],
	"--on-destructive": lift(DARK_STEPS.canvas),
	"--p1": tailwind.blue[300],
	"--p2": tailwind.teal[300],
	"--p3": tailwind.violet[300],
	"--p4": tailwind.lime[300],
};
