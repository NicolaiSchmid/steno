/**
 * The kit's one focus style: a 2 px `--focus` ring (3:1 on every surface)
 * drawn outside the control on a 2 px offset in the colour of the surface
 * it sits on (`--surface`), so the ring is measured against that surface
 * and not against the control's own fill. No border change beside it.
 */
export const focusRingClass =
	"outline-none focus-visible:ring-2 focus-visible:ring-focus focus-visible:ring-offset-2 focus-visible:ring-offset-surface";

/** The same ring for a frame whose focus is on a field inside it. */
export const focusWithinRingClass =
	"focus-within:ring-2 focus-within:ring-focus focus-within:ring-offset-2 focus-within:ring-offset-surface";
