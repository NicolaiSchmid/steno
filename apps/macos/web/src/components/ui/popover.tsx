import { Popover as BasePopover } from "@base-ui/react/popover";
import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";
import { popupSurfaceClass } from "./menu";

/**
 * A glass surface anchored to its trigger for a small form or explanation.
 * Unlike menus, popovers scale in from 0.98 at the surface tempo.
 */
export const Popover = BasePopover.Root;

export type PopoverProps = ComponentProps<typeof BasePopover.Root>;

export interface PopoverTriggerProps
	extends Omit<ComponentProps<typeof BasePopover.Trigger>, "className"> {
	className?: string;
}

export function PopoverTrigger(props: PopoverTriggerProps) {
	return <BasePopover.Trigger {...props} />;
}

export const popoverPopupVariants = cva(
	[
		popupSurfaceClass,
		"edge-highlight origin-(--transform-origin) transition-[opacity,scale] duration-(--duration-surface) ease-standard",
		"data-starting-style:scale-[0.98] data-starting-style:opacity-0",
		"data-ending-style:scale-[0.98] data-ending-style:opacity-0",
	],
	{
		variants: {
			size: {
				sm: "w-64",
				md: "w-80",
				lg: "w-96",
			},
			padding: {
				md: "p-4",
				sm: "px-3 py-2",
			},
		},
		defaultVariants: { size: "md", padding: "md" },
	},
);

export interface PopoverPopupProps
	extends Omit<ComponentProps<typeof BasePopover.Popup>, "className">,
		VariantProps<typeof popoverPopupVariants> {
	className?: string;
	side?: ComponentProps<typeof BasePopover.Positioner>["side"];
	align?: ComponentProps<typeof BasePopover.Positioner>["align"];
	sideOffset?: number;
	container?: ComponentProps<typeof BasePopover.Portal>["container"];
}

export function PopoverPopup({
	className,
	size,
	padding,
	side = "bottom",
	align = "center",
	sideOffset = 4,
	container,
	...props
}: PopoverPopupProps) {
	return (
		<BasePopover.Portal container={container}>
			<BasePopover.Positioner
				align={align}
				className="z-50 outline-none"
				side={side}
				sideOffset={sideOffset}
			>
				<BasePopover.Popup
					className={cn(popoverPopupVariants({ size, padding }), className)}
					{...props}
				/>
			</BasePopover.Positioner>
		</BasePopover.Portal>
	);
}

export function PopoverTitle({
	className,
	...props
}: ComponentProps<typeof BasePopover.Title> & { className?: string }) {
	return (
		<BasePopover.Title
			className={cn("m-0 font-semibold text-sm leading-none", className)}
			{...props}
		/>
	);
}

export function PopoverDescription({
	className,
	...props
}: ComponentProps<typeof BasePopover.Description> & { className?: string }) {
	return (
		<BasePopover.Description
			className={cn("mt-2 mb-0 text-muted-foreground text-sm", className)}
			{...props}
		/>
	);
}
