import { Easing } from "react-native-reanimated";

/**
 * The app's motion tokens — one tempo system, consumed everywhere. No inline
 * durations at call sites (plan: .plans/2026-08-22-ios-motion-polish.md).
 *
 * Three registers:
 * - functional (opacity/color state swaps): fast, curve-driven;
 * - entrance (content arriving): slightly slower fade;
 * - spatial (layout moves, morphs, toasts): one critically-damped spring —
 *   position changes spring, opacity changes ease. Never both tempos on the
 *   same property.
 */

/** Standard curve for every timing-driven change (web's `ease` equivalent). */
export const EASE_STANDARD = Easing.bezier(0.4, 0, 0.2, 1);

/** Opacity/color swaps between existing states. */
export const DURATION_FUNCTIONAL = 150;

/** Exits are quicker than entries so the incoming state reads first. */
export const DURATION_EXIT = 120;

/** Content arriving (loading → loaded, rows appearing, notices mounting). */
export const DURATION_ENTRANCE = 250;

/** The one spatial spring: ~250ms settle, no visible bounce. */
export const SPRING_SPATIAL = { damping: 28, mass: 1, stiffness: 320 } as const;

/** Pressed-state targets shared by every interactive control. */
export const PRESS_SCALE = 0.97;
export const PRESS_OPACITY = 0.85;
/** Press-in is near-instant; release relaxes at the functional tempo. */
export const DURATION_PRESS_IN = 100;

/** One hit-slop token — replaces the 6/8/10/12 spread across the app. */
export const HIT_SLOP = 10;
