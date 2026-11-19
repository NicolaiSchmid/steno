import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

/**
 * The panels' shared pieces, the Swift `RecordingBubbleView.swift`
 * primitives in the web language: the pill (`PanelBar`), the five live
 * bars and the draining countdown hairline. One hue on every surface;
 * the bars are achromatic.
 */

/**
 * The pill: popover fill (opaque in both appearances), a hairline border,
 * the `2xl` radius (the web scale's nearest to the Swift `Theme.Radius.xl`).
 * No CSS shadow: the window is the pill's size and would clip it to a grey
 * band; on macOS the panel's own shadow follows the pill's rounded alpha.
 * `data-tauri-drag-region="deep"` drags the panel from anywhere in the
 * pill, its text and bars included; buttons stop the drag by being
 * buttons, and the bubble's body takes a drag through `useClickOrDrag`.
 */
export function PanelBar({ className, ...props }: ComponentProps<"div">) {
	return (
		<div
			className={cn(
				"surface-popover inline-flex items-center rounded-2xl border border-border bg-popover text-foreground",
				className,
			)}
			data-tauri-drag-region="deep"
			{...props}
		/>
	);
}

/** The last five level samples as bar fractions, newest last. */
export const LIVE_BARS = 5;

/** Adds a level (0 to 1) to a five-sample history. */
export function pushLevel(history: readonly number[], level: number): number[] {
	const next = [...history, Math.min(1, Math.max(0, level))];
	return next.slice(-LIVE_BARS);
}

export const EMPTY_HISTORY: readonly number[] = Array.from(
	{ length: LIVE_BARS },
	() => 0,
);

/** Five 3 px bars, heights 4 to 16 px from the history, newest trailing. */
export function LiveBars({ history }: { history: readonly number[] }) {
	return (
		<span
			aria-hidden="true"
			className="flex h-4 items-center gap-0.5"
			data-testid="live-bars"
		>
			{history.map((fraction, index) => (
				<span
					className="relative block h-4 w-[3px] overflow-hidden rounded-full bg-border"
					// The position is the identity: the bars shift left as samples arrive.
					// biome-ignore lint/suspicious/noArrayIndexKey: fixed slots
					key={index}
				>
					<span
						className="absolute inset-x-0 bottom-0 block rounded-full bg-foreground transition-[height] duration-(--duration-functional) ease-standard motion-reduce:transition-none"
						style={{ height: `${4 + 12 * fraction}px` }}
					/>
				</span>
			))}
		</span>
	);
}

/**
 * The draining countdown: a 2 px track with a fill whose width is the
 * fraction remaining, stepping once a second (no animation under reduced
 * motion).
 */
export function CountdownHairline({
	fractionRemaining,
}: {
	fractionRemaining: number;
}) {
	const fraction = Math.min(1, Math.max(0, fractionRemaining));
	return (
		<span
			aria-hidden="true"
			className="absolute inset-x-3 bottom-1 block h-0.5 overflow-hidden rounded-full bg-border"
			data-testid="countdown-hairline"
		>
			<span
				className="block h-full rounded-full bg-countdown transition-[width] duration-(--duration-countdown-step) ease-linear motion-reduce:transition-none"
				style={{ width: `${fraction * 100}%` }}
			/>
		</span>
	);
}
