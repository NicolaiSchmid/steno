import { Tabs as BaseTabs } from "@base-ui/react/tabs";
import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";
import { focusRingClass } from "./focus-ring";

/**
 * Underline tabs: the labels in the muted colour, the active one in the
 * foreground with a 2 px bar under it (`--tab-indicator`, 3:1 on the
 * canvas). No track and no outline. When the column is too narrow for every
 * tab the row scrolls sideways; its 4 px padding keeps the focus ring
 * inside the scroll box, and a negative margin keeps the first label on the
 * column's edge.
 */
export const Tabs = BaseTabs.Root;

export type TabsProps = ComponentProps<typeof BaseTabs.Root>;

export interface TabsListProps
	extends Omit<ComponentProps<typeof BaseTabs.List>, "className"> {
	className?: string;
}

export function TabsList({
	className,
	activateOnFocus = true,
	...props
}: TabsListProps) {
	return (
		<BaseTabs.List
			activateOnFocus={activateOnFocus}
			className={cn(
				"-m-1 flex min-w-0 max-w-[calc(100%+0.5rem)] gap-3 overflow-x-auto p-1 [scrollbar-width:thin]",
				className,
			)}
			{...props}
		/>
	);
}

export interface TabsTabProps
	extends Omit<ComponentProps<typeof BaseTabs.Tab>, "className"> {
	className?: string;
	/** A small count after the label, for example the number of turns. */
	count?: number | string | undefined;
}

export function TabsTab({
	className,
	count,
	children,
	...props
}: TabsTabProps) {
	return (
		<BaseTabs.Tab
			className={cn(
				"relative inline-flex h-8 shrink-0 cursor-default select-none items-center gap-1 whitespace-nowrap rounded-xs px-1 text-label text-muted-foreground outline-none",
				"transition-colors duration-(--duration-functional) ease-standard hover:text-foreground",
				"after:absolute after:inset-x-1 after:bottom-0 after:h-0.5 after:rounded-full after:content-['']",
				focusRingClass,
				"data-active:text-foreground data-active:after:bg-tab-indicator",
				className,
			)}
			{...props}
		>
			{children}
			{count !== undefined ? (
				<small className="text-caption text-muted-foreground tabular-nums">
					{count}
				</small>
			) : null}
		</BaseTabs.Tab>
	);
}

export const tabsPanelVariants = cva("outline-none", {
	variants: {
		variant: {
			default: "",
			/** Long-form content: the reading type, 15 px on a 1.6 line height. */
			reading: "text-reading",
		},
	},
	defaultVariants: { variant: "default" },
});

export interface TabsPanelProps
	extends Omit<ComponentProps<typeof BaseTabs.Panel>, "className">,
		VariantProps<typeof tabsPanelVariants> {
	className?: string;
	children?: ReactNode;
}

export function TabsPanel({ className, variant, ...props }: TabsPanelProps) {
	return (
		<BaseTabs.Panel
			className={cn(tabsPanelVariants({ variant }), className)}
			{...props}
		/>
	);
}
