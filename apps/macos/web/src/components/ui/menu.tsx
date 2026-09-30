import { Menu as BaseMenu } from "@base-ui/react/menu";
import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";
import { Kbd } from "./kbd";

/**
 * The actions menu: a glass popup with the deep popover shadow. The popup
 * mounts on open and unmounts on close (never `display: none`, which blanks
 * headless Chromium under `backdrop-filter`).
 */
export const Menu = BaseMenu.Root;

export type MenuProps = ComponentProps<typeof BaseMenu.Root>;

export interface MenuTriggerProps
	extends Omit<ComponentProps<typeof BaseMenu.Trigger>, "className"> {
	className?: string;
}

/** Wrap a `Button` with `render`: `<MenuTrigger render={<Button variant="glass" />} />`. */
export function MenuTrigger(props: MenuTriggerProps) {
	return <BaseMenu.Trigger {...props} />;
}

export const popupSurfaceClass = cn(
	"dropdown-glass rounded-lg border p-1 text-foreground shadow-pop outline-none",
	"origin-(--transform-origin) transition-[opacity,transform] duration-(--duration-surface) ease-standard",
	"data-starting-style:scale-[0.98] data-starting-style:opacity-0",
	"data-ending-style:scale-[0.98] data-ending-style:opacity-0",
);

export interface MenuPopupProps
	extends Omit<ComponentProps<typeof BaseMenu.Popup>, "className"> {
	className?: string;
	side?: ComponentProps<typeof BaseMenu.Positioner>["side"];
	align?: ComponentProps<typeof BaseMenu.Positioner>["align"];
	sideOffset?: number;
	/** Where to portal; defaults to the document body. */
	container?: ComponentProps<typeof BaseMenu.Portal>["container"];
}

export function MenuPopup({
	className,
	side = "bottom",
	align = "end",
	sideOffset = 6,
	container,
	...props
}: MenuPopupProps) {
	return (
		<BaseMenu.Portal container={container}>
			<BaseMenu.Positioner
				align={align}
				className="z-50 outline-none"
				side={side}
				sideOffset={sideOffset}
			>
				<BaseMenu.Popup
					className={cn(popupSurfaceClass, "min-w-[220px]", className)}
					{...props}
				/>
			</BaseMenu.Positioner>
		</BaseMenu.Portal>
	);
}

export const menuItemVariants = cva(
	[
		"flex h-[30px] cursor-default select-none items-center gap-2 rounded-[6px] px-2 text-[13px] outline-none",
		"[&>svg]:size-4 [&>svg]:shrink-0 [&>svg]:stroke-[1.75]",
		"data-highlighted:bg-accent data-disabled:opacity-50",
	],
	{
		variants: {
			variant: {
				default: "text-foreground [&>svg]:text-muted-foreground",
				destructive: "text-destructive [&>svg]:text-destructive",
			},
		},
		defaultVariants: { variant: "default" },
	},
);

export interface MenuItemProps
	extends Omit<ComponentProps<typeof BaseMenu.Item>, "className">,
		VariantProps<typeof menuItemVariants> {
	className?: string;
	icon?: ReactNode;
	/** A keyboard hint shown at the trailing edge, for example "⇧⌘E". */
	shortcut?: ReactNode;
}

export function MenuItem({
	className,
	variant,
	icon,
	shortcut,
	children,
	...props
}: MenuItemProps) {
	return (
		<BaseMenu.Item
			className={cn(menuItemVariants({ variant }), className)}
			{...props}
		>
			{icon}
			<span className="min-w-0 flex-1 truncate">{children}</span>
			{shortcut ? <MenuShortcut>{shortcut}</MenuShortcut> : null}
		</BaseMenu.Item>
	);
}

export function MenuShortcut({
	className,
	...props
}: ComponentProps<typeof Kbd>) {
	return (
		<Kbd className={cn("ml-auto", className)} variant="plain" {...props} />
	);
}

export interface MenuSeparatorProps
	extends Omit<ComponentProps<typeof BaseMenu.Separator>, "className"> {
	className?: string;
}

export function MenuSeparator({ className, ...props }: MenuSeparatorProps) {
	return (
		<BaseMenu.Separator
			className={cn("mx-2 my-1 h-px bg-border", className)}
			{...props}
		/>
	);
}
