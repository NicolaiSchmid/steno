import { Menu as BaseMenu } from "@base-ui/react/menu";
import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";
import { Kbd } from "./kbd";

/**
 * The actions menu: a glass popup with the deep popover shadow. The popup
 * mounts on open and unmounts on close (never `display: none`, which blanks
 * headless Chromium under `backdrop-filter`). Menus appear in place;
 * popovers and tooltips are the surfaces that scale in.
 */
export const Menu = BaseMenu.Root;

export type MenuProps = ComponentProps<typeof BaseMenu.Root>;

export interface MenuTriggerProps
	extends Omit<ComponentProps<typeof BaseMenu.Trigger>, "className"> {
	className?: string;
}

/** Wrap a `Button` with `render`: `<MenuTrigger render={<Button variant="ghost" />} />`. */
export function MenuTrigger(props: MenuTriggerProps) {
	return <BaseMenu.Trigger {...props} />;
}

/** The glass popup surface shared by menus, selects and popovers; padding is the caller's. */
export const popupSurfaceClass =
	"dropdown-glass rounded-lg border text-foreground shadow-pop outline-none";

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
	sideOffset = 4,
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
					className={cn(popupSurfaceClass, "min-w-44 p-1", className)}
					{...props}
				/>
			</BaseMenu.Positioner>
		</BaseMenu.Portal>
	);
}

/** A 28 px row: icon at 80 percent, the words, a shortcut at the trailing edge. */
export const menuItemVariants = cva(
	[
		"flex min-h-7 cursor-default select-none items-center gap-2 rounded-sm px-2 py-1 text-sm outline-none",
		"[&>svg]:-mx-0.5 [&>svg]:size-4 [&>svg]:shrink-0 [&>svg]:opacity-80",
		"data-highlighted:bg-accent data-disabled:opacity-64",
	],
	{
		variants: {
			variant: {
				default: "text-foreground [&>svg]:text-muted-foreground",
				destructive: "text-destructive-foreground [&>svg]:text-current",
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
