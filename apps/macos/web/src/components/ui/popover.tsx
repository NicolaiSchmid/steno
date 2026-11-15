import { Popover as BasePopover } from "@base-ui/react/popover";
import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";
import { popupSurfaceClass } from "./menu";

/**
 * A popup surface anchored to its trigger for a small form or explanation.
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
		"origin-(--transform-origin) transition-[opacity,scale] duration-(--duration-surface) ease-standard",
		"data-starting-style:scale-[0.98] data-starting-style:opacity-0",
		"data-ending-style:scale-[0.98] data-ending-style:opacity-0",
	],
	{
		variants: {
			size: {
				sm: "w-64",
				md: "w-80",
				lg: "w-96",
				/**
				 * The speakers panel: 368 px, at most 440 px tall and never
				 * past the space the positioner leaves (`--available-*`, a
				 * zoomed or short window), scrolling on its own.
				 */
				panel:
					"max-h-[min(27.5rem,var(--available-height))] w-92 max-w-(--available-width) overflow-y-auto overscroll-contain",
				/** The transcript's speaker panel: 320 px, inside the reading column. */
				picker:
					"max-h-(--available-height) w-80 max-w-(--available-width) overflow-y-auto overscroll-contain",
			},
			padding: {
				md: "p-4",
				sm: "px-3 py-2",
				panel: "px-2 py-2",
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
	/** What the popup is placed against, when not its trigger. */
	anchor?: ComponentProps<typeof BasePopover.Positioner>["anchor"];
	/**
	 * How the popup avoids the window's edges. A popup that scrolls on its
	 * own passes `fallbackAxisSide: "none"`, so it keeps to its side and
	 * takes the height there (`--available-height`) instead of moving
	 * beside its trigger.
	 */
	collisionAvoidance?: ComponentProps<
		typeof BasePopover.Positioner
	>["collisionAvoidance"];
	container?: ComponentProps<typeof BasePopover.Portal>["container"];
}

export function PopoverPopup({
	className,
	size,
	padding,
	side = "bottom",
	align = "center",
	sideOffset = 4,
	anchor,
	collisionAvoidance,
	container,
	...props
}: PopoverPopupProps) {
	return (
		<BasePopover.Portal container={container}>
			<BasePopover.Positioner
				align={align}
				{...(anchor ? { anchor } : {})}
				className="z-50 outline-none"
				{...(collisionAvoidance ? { collisionAvoidance } : {})}
				collisionPadding={8}
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
		<BasePopover.Title className={cn("m-0 text-title", className)} {...props} />
	);
}

export function PopoverDescription({
	className,
	...props
}: ComponentProps<typeof BasePopover.Description> & { className?: string }) {
	return (
		<BasePopover.Description
			className={cn("mt-2 mb-0 text-body text-muted-foreground", className)}
			{...props}
		/>
	);
}
