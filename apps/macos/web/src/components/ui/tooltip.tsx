import { Tooltip as BaseTooltip } from "@base-ui/react/tooltip";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";

/** Put one `TooltipProvider` at the app root so hover delays are shared. */
export const TooltipProvider = BaseTooltip.Provider;

export interface TooltipProps
	extends Omit<ComponentProps<typeof BaseTooltip.Root>, "children"> {
	/** The trigger element; it receives the tooltip's hover and focus props. */
	children: ComponentProps<typeof BaseTooltip.Trigger>["render"];
	label: ReactNode;
	side?: ComponentProps<typeof BaseTooltip.Positioner>["side"];
	sideOffset?: number;
	container?: ComponentProps<typeof BaseTooltip.Portal>["container"];
}

/** An opaque popover-coloured hint (not glass) that scales in from 0.98. */
export function Tooltip({
	children,
	label,
	side = "top",
	sideOffset = 4,
	container,
	...props
}: TooltipProps) {
	return (
		<BaseTooltip.Root {...props}>
			<BaseTooltip.Trigger render={children} />
			<BaseTooltip.Portal container={container}>
				<BaseTooltip.Positioner
					className="z-50 outline-none"
					side={side}
					sideOffset={sideOffset}
				>
					<BaseTooltip.Popup
						className={cn(
							"relative max-w-80 rounded-md border border-border bg-popover px-2 py-1 text-foreground text-xs shadow-md outline-none",
							"origin-(--transform-origin) transition-[opacity,scale] duration-(--duration-functional) ease-standard",
							"before:pointer-events-none before:absolute before:inset-0 before:rounded-[calc(var(--radius-md)-1px)] before:shadow-[var(--edge-highlight)]",
							"data-starting-style:scale-[0.98] data-starting-style:opacity-0",
							"data-ending-style:scale-[0.98] data-ending-style:opacity-0",
						)}
					>
						{label}
					</BaseTooltip.Popup>
				</BaseTooltip.Positioner>
			</BaseTooltip.Portal>
		</BaseTooltip.Root>
	);
}
