import { Tabs as BaseTabs } from "@base-ui/react/tabs";
import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";

/**
 * Segmented tabs: a 2 px frame on the field tint, each tab a 24 px pill;
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
				"inline-flex w-max shrink-0 gap-0.5 rounded-lg bg-input/40 p-0.5",
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
				"inline-flex h-6 min-w-0 cursor-default select-none items-center gap-1 rounded-md px-2.5 font-medium text-muted-foreground text-xs outline-none",
				"transition-[background-color,color,box-shadow] duration-(--duration-functional) ease-standard",
				"hover:bg-background/55 hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring",
				"data-active:bg-background data-active:text-foreground data-active:shadow-xs",
				"dark:data-active:bg-input/72 dark:hover:bg-input/32",
				className,
			)}
			{...props}
		>
			{children}
			{count !== undefined ? (
				<small className="font-semibold text-3xs text-muted-foreground/70 tabular-nums">
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
			/** Long-form content: 15 px on a 1.6 line height. */
			reading: "text-[15px] leading-[1.6]",
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
