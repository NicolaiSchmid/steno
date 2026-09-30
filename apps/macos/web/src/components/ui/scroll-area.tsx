import { ScrollArea as BaseScrollArea } from "@base-ui/react/scroll-area";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

export interface ScrollAreaProps
	extends Omit<ComponentProps<typeof BaseScrollArea.Root>, "className"> {
	className?: string;
	/** Layout classes for the scrolling viewport (padding belongs to children). */
	viewportClassName?: string;
	orientation?: "vertical" | "horizontal" | "both";
}

/**
 * A scroll container with a thin overlay scrollbar that shows while the
 * pointer is over it or the content moves.
 */
export function ScrollArea({
	className,
	viewportClassName,
	orientation = "vertical",
	children,
	...props
}: ScrollAreaProps) {
	const scrollbarClass = cn(
		"flex touch-none select-none p-0.5 opacity-0 transition-opacity duration-(--duration-functional) ease-standard",
		"data-hovering:opacity-100 data-scrolling:opacity-100",
	);
	return (
		<BaseScrollArea.Root
			className={cn("relative min-h-0 min-w-0 overflow-hidden", className)}
			{...props}
		>
			<BaseScrollArea.Viewport
				className={cn(
					"size-full overscroll-contain outline-none",
					viewportClassName,
				)}
			>
				{children}
			</BaseScrollArea.Viewport>
			{orientation !== "horizontal" ? (
				<BaseScrollArea.Scrollbar
					className={cn(scrollbarClass, "absolute inset-y-1 right-0.5 w-2")}
					orientation="vertical"
				>
					<BaseScrollArea.Thumb className="w-full flex-1 rounded-full bg-foreground/25" />
				</BaseScrollArea.Scrollbar>
			) : null}
			{orientation !== "vertical" ? (
				<BaseScrollArea.Scrollbar
					className={cn(scrollbarClass, "absolute inset-x-1 bottom-0.5 h-2")}
					orientation="horizontal"
				>
					<BaseScrollArea.Thumb className="h-full flex-1 rounded-full bg-foreground/25" />
				</BaseScrollArea.Scrollbar>
			) : null}
		</BaseScrollArea.Root>
	);
}
