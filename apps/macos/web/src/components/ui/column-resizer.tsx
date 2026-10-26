import {
	type ComponentProps,
	type KeyboardEvent,
	type PointerEvent,
	useRef,
	useState,
} from "react";
import { cn } from "@/lib/cn";

/** Pixels an arrow key moves the divider; Shift moves it four times as far. */
const KEY_STEP = 8;

export interface ColumnResizerProps
	extends Omit<ComponentProps<"div">, "onChange" | "children"> {
	/** The column's width in CSS pixels. */
	value: number;
	min: number;
	max: number;
	/** The width a double-click puts back. */
	defaultValue: number;
	/** Every change while dragging or on a key, already clamped. */
	onChange: (width: number) => void;
	/** Once a drag ends, a key moves the divider, or a double-click resets it. */
	onCommit?: (width: number) => void;
	/** Names what the divider resizes ("Resize sidebar"). */
	"aria-label": string;
}

function clamp(value: number, min: number, max: number) {
	return Math.round(Math.min(Math.max(value, min), Math.max(min, max)));
}

/**
 * The divider on the trailing edge of a column: a 9 px strip over the
 * column's hairline that reaches only 2 px into the column (a scroll bar at
 * its edge stays grabbable) and the rest into the next one. Invisible until
 * hovered (a col-resize cursor and a hairline in the ring colour), dragged
 * with pointer capture (a move without the button down ends it), a focusable
 * `separator` that arrow keys, Home and End move, and a double-click puts
 * back to `defaultValue`. Its parent is the column, positioned `relative`.
 */
export function ColumnResizer({
	value,
	min,
	max,
	defaultValue,
	onChange,
	onCommit,
	className,
	...props
}: ColumnResizerProps) {
	const drag = useRef<{ x: number; width: number; last: number } | null>(null);
	const [dragging, setDragging] = useState(false);

	const set = (width: number, commit: boolean) => {
		const next = clamp(width, min, max);
		// A key that cannot move a narrowed column leaves the stored width alone.
		if (next !== value) {
			onChange(next);
			if (commit) onCommit?.(next);
		}
		return next;
	};

	const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
		if (event.button !== 0 || event.ctrlKey) return;
		event.preventDefault();
		event.currentTarget.setPointerCapture?.(event.pointerId);
		drag.current = { x: event.clientX, width: value, last: value };
		setDragging(true);
	};
	const endDrag = (event: PointerEvent<HTMLDivElement>) => {
		const start = drag.current;
		if (!start) return;
		drag.current = null;
		setDragging(false);
		event.currentTarget.releasePointerCapture?.(event.pointerId);
		if (start.last !== start.width) onCommit?.(start.last);
	};
	const onPointerMove = (event: PointerEvent<HTMLDivElement>) => {
		const start = drag.current;
		if (!start) return;
		// The release went missing (a context menu, a lost window): end here.
		if ((event.buttons & 1) === 0) {
			endDrag(event);
			return;
		}
		start.last = set(start.width + event.clientX - start.x, false);
	};
	const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
		const step = event.shiftKey ? KEY_STEP * 4 : KEY_STEP;
		const target = {
			ArrowLeft: value - step,
			ArrowRight: value + step,
			Home: min,
			End: max,
		}[event.key];
		if (target === undefined) return;
		event.preventDefault();
		set(target, true);
	};

	return (
		// biome-ignore lint/a11y/useSemanticElements: an <hr> cannot take focus or a value; a focusable separator is the ARIA window splitter pattern.
		<div
			aria-orientation="vertical"
			aria-valuemax={Math.max(min, max)}
			aria-valuemin={min}
			aria-valuenow={value}
			className={cn(
				"absolute inset-y-0 -right-[7px] z-20 w-[9px] cursor-col-resize touch-none select-none outline-none",
				"after:absolute after:inset-y-0 after:left-px after:w-px after:bg-transparent",
				"after:transition-[background-color] after:duration-(--duration-functional) after:ease-standard",
				"hover:after:bg-ring/30 focus-visible:after:bg-ring/60",
				dragging && "after:bg-ring/60",
				className,
			)}
			onDoubleClick={() => {
				// Stored even when a narrow window clamps it, so it is what comes back.
				set(defaultValue, false);
				onCommit?.(defaultValue);
			}}
			onKeyDown={onKeyDown}
			onLostPointerCapture={endDrag}
			onPointerCancel={endDrag}
			onPointerDown={onPointerDown}
			onPointerMove={onPointerMove}
			onPointerUp={endDrag}
			role="separator"
			tabIndex={0}
			{...props}
		/>
	);
}
