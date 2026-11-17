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

/**
 * The focus style for a row in a menu or a select's list, where the rows
 * touch and a ring outside would sit on the next row: the same 2 px
 * `--focus` line drawn just inside the row, over its highlight (the accent
 * fill, or the selected fill of a select's chosen option). Both are measured
 * at 3:1 on the popover. Only for keyboard focus; the fill alone follows the
 * pointer.
 */
export const focusInsetClass =
	"focus-visible:outline-2 focus-visible:outline-focus focus-visible:outline-solid focus-visible:-outline-offset-2";

/**
 * The same inset line on the highlighted option of a combobox whose focus
 * stays in its field (`aria-activedescendant`), so the option itself is
 * never focused: drawn while the listbox carries `data-keys`, that is while
 * the keys, not the pointer, moved the highlight.
 */
export const activeInsetClass =
	"in-data-keys:data-highlighted:outline-2 in-data-keys:data-highlighted:outline-focus in-data-keys:data-highlighted:outline-solid in-data-keys:data-highlighted:-outline-offset-2";

/**
 * The ring inside a row of a scrolling list (the speakers panel), where an
 * outside ring would be clipped by the scroll: drawn inset, on the row's
 * own fill, which the contrast test measures as a row host.
 */
export const focusRowClass =
	"outline-none focus-visible:ring-2 focus-visible:ring-focus focus-visible:ring-inset";
