import { Popover as BasePopover } from "@base-ui/react/popover";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";
import { popupSurfaceClass } from "./menu";

/** A glass surface anchored to its trigger for a small form or explanation. */
export const Popover = BasePopover.Root;

export type PopoverProps = ComponentProps<typeof BasePopover.Root>;

export interface PopoverTriggerProps
	extends Omit<ComponentProps<typeof BasePopover.Trigger>, "className"> {
	className?: string;
}

export function PopoverTrigger(props: PopoverTriggerProps) {
	return <BasePopover.Trigger {...props} />;
}

export interface PopoverPopupProps
	extends Omit<ComponentProps<typeof BasePopover.Popup>, "className"> {
	className?: string;
	side?: ComponentProps<typeof BasePopover.Positioner>["side"];
	align?: ComponentProps<typeof BasePopover.Positioner>["align"];
	sideOffset?: number;
	container?: ComponentProps<typeof BasePopover.Portal>["container"];
}

export function PopoverPopup({
	className,
	side = "bottom",
	align = "center",
	sideOffset = 8,
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
					className={cn(popupSurfaceClass, "w-72 p-3.5", className)}
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
			className={cn("m-0 font-medium text-[13px]", className)}
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
			className={cn(
				"mt-1 mb-0 text-[12.5px] text-muted-foreground leading-[1.45]",
				className,
			)}
			{...props}
		/>
	);
}
