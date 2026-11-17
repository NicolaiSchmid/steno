import { Tabs as BaseTabs } from "@base-ui/react/tabs";
import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";
import { focusRingClass } from "./focus-ring";

/**
 * Segmented tabs: a 2 px frame on the field tint, each tab a 24 px segment;
 * the active one lifts onto the canvas colour (a translucent lift in dark).
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
				"inline-flex w-max shrink-0 gap-0.5 rounded-lg bg-tab-track p-0.5",
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
				"inline-flex h-6 min-w-0 cursor-default select-none items-center gap-1 rounded-md px-2.5 text-label text-muted-foreground outline-none",
				"transition-[background-color,color,box-shadow] duration-(--duration-functional) ease-standard",
				"hover:bg-tab-hover hover:text-foreground",
				focusRingClass,
				"data-active:inset-ring data-active:inset-ring-tab-indicator data-active:bg-tab-active data-active:text-foreground",
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
