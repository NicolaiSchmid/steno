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
 * The divider on the trailing edge of a column: a 9 px strip centred on the
 * column's hairline, invisible until hovered (a col-resize cursor and a
 * hairline in the ring colour), dragged with pointer capture, a focusable
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
		if (next !== value) onChange(next);
		if (commit) onCommit?.(next);
		return next;
	};

	const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
		if (event.button !== 0) return;
		event.preventDefault();
		event.currentTarget.setPointerCapture?.(event.pointerId);
		drag.current = { x: event.clientX, width: value, last: value };
		setDragging(true);
	};
	const onPointerMove = (event: PointerEvent<HTMLDivElement>) => {
		const start = drag.current;
		if (!start) return;
		start.last = set(start.width + event.clientX - start.x, false);
	};
	const endDrag = (event: PointerEvent<HTMLDivElement>) => {
		const start = drag.current;
		if (!start) return;
		drag.current = null;
		setDragging(false);
		event.currentTarget.releasePointerCapture?.(event.pointerId);
		if (start.last !== start.width) onCommit?.(start.last);
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
				"absolute inset-y-0 right-0 z-20 w-[9px] translate-x-1/2 cursor-col-resize touch-none select-none outline-none",
				"after:absolute after:inset-y-0 after:left-1/2 after:w-px after:-translate-x-1/2 after:bg-transparent",
				"after:transition-[background-color] after:duration-(--duration-functional) after:ease-standard",
				"hover:after:bg-ring/30 focus-visible:after:bg-ring/60",
				dragging && "after:bg-ring/60",
				className,
			)}
			data-dragging={dragging ? "" : undefined}
			onDoubleClick={() => set(defaultValue, true)}
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
