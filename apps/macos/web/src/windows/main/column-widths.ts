import { useState } from "react";

/**
 * The main window's two resizable columns, their defaults and limits, and
 * where the widths are kept between launches.
 *
 * The widths live in the page's `localStorage`. The Tauri shell's web view
 * keeps it across launches on every OS; the Swift app's web view has no
 * persistent storage (`WebWindowView`), so there they hold until the window
 * closes.
 */
export const COLUMNS = {
	sidebar: { default: 256, min: 200, max: 360 },
	list: { default: 300, min: 240, max: 480 },
} as const;

/** The reading column never gets narrower than this while a divider moves. */
export const DETAIL_MIN = 400;

export const COLUMN_WIDTHS_KEY = "steno.main.columnWidths";

export type ColumnName = keyof typeof COLUMNS;
export type ColumnWidths = Record<ColumnName, number>;

const defaults: ColumnWidths = {
	sidebar: COLUMNS.sidebar.default,
	list: COLUMNS.list.default,
};

function inRange(name: ColumnName, value: unknown): value is number {
	const { min, max } = COLUMNS[name];
	return typeof value === "number" && value >= min && value <= max;
}

/** The stored widths, each falling back to its default when missing or out of range. */
export function loadColumnWidths(): ColumnWidths {
	try {
		const stored = JSON.parse(
			window.localStorage.getItem(COLUMN_WIDTHS_KEY) ?? "{}",
		) as Partial<Record<ColumnName, unknown>>;
		return {
			sidebar: inRange("sidebar", stored.sidebar)
				? stored.sidebar
				: defaults.sidebar,
			list: inRange("list", stored.list) ? stored.list : defaults.list,
		};
	} catch {
		return { ...defaults };
	}
}

function saveColumnWidths(widths: ColumnWidths) {
	try {
		window.localStorage.setItem(COLUMN_WIDTHS_KEY, JSON.stringify(widths));
	} catch {
		// Storage refused (full, or switched off): the widths last this session.
	}
}

/** The widths, a setter for a drag in progress, and one that also stores them. */
export function useColumnWidths() {
	const [widths, setWidths] = useState(loadColumnWidths);
	const update = (name: ColumnName, width: number) =>
		setWidths((current) => ({ ...current, [name]: width }));
	const commit = (name: ColumnName, width: number) => {
		const next = { ...widths, [name]: width };
		setWidths(next);
		saveColumnWidths(next);
	};
	return { widths, update, commit };
}

/**
 * The widest `name` may be in a window `windowWidth` wide: its own limit, or
 * less so that the reading column keeps `DETAIL_MIN`. Never below its minimum.
 */
export function columnMax(
	name: ColumnName,
	widths: ColumnWidths,
	windowWidth: number,
) {
	const other = name === "sidebar" ? widths.list : widths.sidebar;
	const room = windowWidth > 0 ? windowWidth - other - DETAIL_MIN : Infinity;
	return Math.max(COLUMNS[name].min, Math.min(COLUMNS[name].max, room));
}

/**
 * The widths as drawn in a window `windowWidth` wide: the stored ones, or,
 * when they leave the reading column less than `DETAIL_MIN`, the list and
 * then the sidebar narrowed towards their minimums. The stored widths come
 * back once the window is wide enough again.
 */
export function fitColumns(
	widths: ColumnWidths,
	windowWidth: number,
): ColumnWidths {
	let over = widths.sidebar + widths.list + DETAIL_MIN - windowWidth;
	if (windowWidth <= 0 || over <= 0) return widths;
	const list = Math.max(COLUMNS.list.min, widths.list - over);
	over -= widths.list - list;
	const sidebar = Math.max(COLUMNS.sidebar.min, widths.sidebar - over);
	return { sidebar, list };
}
