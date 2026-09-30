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

export function Tooltip({
	children,
	label,
	side = "top",
	sideOffset = 6,
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
							"dropdown-glass rounded-[7px] border px-2 py-1 text-[12px] text-foreground leading-[1.4] shadow-pop",
							"origin-(--transform-origin) transition-[opacity,transform] duration-(--duration-functional) ease-standard",
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
