/**
 * Motion tokens, ported from the phone app (mobile/src/lib/motion.ts) to CSS
 * time and curves. The values are also declared as custom properties in
 * `src/theme.css` (`--duration-*`, `--ease-*`) so components can reference
 * them from Tailwind (`duration-(--duration-functional)`, `ease-standard`).
 * Nothing at a call site names a duration; `prefers-reduced-motion` is
 * honoured globally in theme.css.
 */

/** Standard curve for every timing-driven change. */
export const EASE_STANDARD = "cubic-bezier(0.4, 0, 0.2, 1)";

/** Surfaces that slide in (drawers, sheets). */
export const EASE_DRAWER = "cubic-bezier(0.32, 0.72, 0, 1)";

/** Opacity and colour swaps between existing states. */
export const DURATION_FUNCTIONAL = 150;

/** Exits are quicker than entries so the incoming state reads first. */
export const DURATION_EXIT = 120;

/** Content arriving: loading to loaded, rows appearing, notices mounting. */
export const DURATION_ENTRANCE = 250;

/** Popups and dialogs open and close: scale 0.98 and opacity (plan, Decision 2). */
export const DURATION_SURFACE = 200;

/** Press-in is near-instant; release relaxes at the functional tempo. */
export const DURATION_PRESS_IN = 100;

/** Pressed-state targets shared by every interactive control. */
export const PRESS_SCALE = 0.97;
export const PRESS_OPACITY = 0.85;

/** The scale popups start and end at. */
export const SURFACE_SCALE = 0.98;

/** The custom property names, for the rare inline style. */
export const motionVars = {
	functional: "--duration-functional",
	exit: "--duration-exit",
	entrance: "--duration-entrance",
	surface: "--duration-surface",
	pressIn: "--duration-press-in",
	easeStandard: "--ease-standard",
	easeDrawer: "--ease-drawer",
} as const;
