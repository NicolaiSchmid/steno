import { ContextMenu as BaseContextMenu } from "@base-ui/react/context-menu";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";
import { Kbd } from "./kbd";
import { menuItemVariants, popupSurfaceClass } from "./menu";

/**
 * The right-click menu on a meeting row: the same glass popup and rows as
 * `Menu`, opened at the pointer. The trigger wraps the row; it renders a
 * `div` unless `render` says otherwise.
 */
export const ContextMenu = BaseContextMenu.Root;

export type ContextMenuProps = ComponentProps<typeof BaseContextMenu.Root>;

export interface ContextMenuTriggerProps
	extends Omit<ComponentProps<typeof BaseContextMenu.Trigger>, "className"> {
	/** Layout classes only; the trigger has no look of its own. */
	className?: string;
}

export function ContextMenuTrigger(props: ContextMenuTriggerProps) {
	return <BaseContextMenu.Trigger {...props} />;
}

export interface ContextMenuPopupProps
	extends Omit<ComponentProps<typeof BaseContextMenu.Popup>, "className"> {
	className?: string;
	container?: ComponentProps<typeof BaseContextMenu.Portal>["container"];
}

export function ContextMenuPopup({
	className,
	container,
	...props
}: ContextMenuPopupProps) {
	return (
		<BaseContextMenu.Portal container={container}>
			<BaseContextMenu.Positioner className="z-50 outline-none">
				<BaseContextMenu.Popup
					className={cn(popupSurfaceClass, "min-w-[200px]", className)}
					{...props}
				/>
			</BaseContextMenu.Positioner>
		</BaseContextMenu.Portal>
	);
}

export interface ContextMenuItemProps
	extends Omit<ComponentProps<typeof BaseContextMenu.Item>, "className"> {
	className?: string;
	variant?: "default" | "destructive";
	icon?: ReactNode;
	shortcut?: ReactNode;
}

export function ContextMenuItem({
	className,
	variant,
	icon,
	shortcut,
	children,
	...props
}: ContextMenuItemProps) {
	return (
		<BaseContextMenu.Item
			className={cn(menuItemVariants({ variant }), className)}
			{...props}
		>
			{icon}
			<span className="min-w-0 flex-1 truncate">{children}</span>
			{shortcut ? (
				<Kbd className="ml-auto" variant="plain">
					{shortcut}
				</Kbd>
			) : null}
		</BaseContextMenu.Item>
	);
}

export interface ContextMenuSeparatorProps
	extends Omit<ComponentProps<typeof BaseContextMenu.Separator>, "className"> {
	className?: string;
}

export function ContextMenuSeparator({
	className,
	...props
}: ContextMenuSeparatorProps) {
	return (
		<BaseContextMenu.Separator
			className={cn("mx-2 my-1 h-px bg-border", className)}
			{...props}
		/>
	);
}
