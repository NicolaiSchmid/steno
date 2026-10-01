import { ScrollArea as BaseScrollArea } from "@base-ui/react/scroll-area";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

export interface ScrollAreaProps
	extends Omit<ComponentProps<typeof BaseScrollArea.Root>, "className"> {
	className?: string;
	/** Layout classes for the scrolling viewport (padding belongs to children). */
	viewportClassName?: string;
	orientation?: "vertical" | "horizontal" | "both";
	/** Fade the top 1.5 rem of content scrolling under a header row. */
	fade?: boolean;
}

const scrollbarClass = cn(
	"flex touch-none select-none opacity-0 transition-opacity delay-300 duration-100",
	"data-hovering:opacity-100 data-scrolling:opacity-100 data-hovering:delay-0 data-scrolling:delay-0",
);
const thumbClass =
	"flex-1 rounded-full bg-(--scrollbar-thumb) hover:bg-(--scrollbar-thumb-hover)";

/**
 * A scroll container with a 6 px overlay scrollbar that fades in while the
 * pointer rests over it or the content moves, and fades out 300 ms after.
 */
export function ScrollArea({
	className,
	viewportClassName,
	orientation = "vertical",
	fade = false,
	children,
	...props
}: ScrollAreaProps) {
	return (
		<BaseScrollArea.Root
			className={cn("relative min-h-0 min-w-0 overflow-hidden", className)}
			{...props}
		>
			<BaseScrollArea.Viewport
				className={cn(
					"size-full overscroll-contain outline-none",
					fade && "scroll-fade-top",
					viewportClassName,
				)}
			>
				{children}
			</BaseScrollArea.Viewport>
			{orientation !== "horizontal" ? (
				<BaseScrollArea.Scrollbar
					className={cn(scrollbarClass, "absolute inset-y-1 right-px w-1.5")}
					orientation="vertical"
				>
					<BaseScrollArea.Thumb className={cn(thumbClass, "w-full")} />
				</BaseScrollArea.Scrollbar>
			) : null}
			{orientation !== "vertical" ? (
				<BaseScrollArea.Scrollbar
					className={cn(scrollbarClass, "absolute inset-x-1 bottom-px h-1.5")}
					orientation="horizontal"
				>
					<BaseScrollArea.Thumb className={cn(thumbClass, "h-full")} />
				</BaseScrollArea.Scrollbar>
			) : null}
		</BaseScrollArea.Root>
	);
}
