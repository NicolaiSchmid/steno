import { Tabs as BaseTabs } from "@base-ui/react/tabs";
import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";

/**
 * Segmented tabs: a pill group on the accent surface, the active tab a
 * raised card (a translucent lift in dark).
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
				"flex w-max shrink-0 gap-1 rounded-lg bg-accent p-[3px]",
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
				"inline-flex h-7 cursor-default select-none items-center gap-1.5 rounded-[7px] px-3 font-medium text-[13px] text-muted-foreground outline-none",
				"transition-[background-color,color,box-shadow] duration-(--duration-functional) ease-standard",
				"hover:text-foreground focus-visible:ring-2 focus-visible:ring-primary/50",
				"data-active:bg-card data-active:text-foreground data-active:shadow-[var(--shadow-xs),0_0_0_1px_var(--border)]",
				"dark:data-active:bg-[rgb(255_255_255/8%)] dark:data-active:shadow-none",
				className,
			)}
			{...props}
		>
			{children}
			{count !== undefined ? (
				<small className="font-normal text-[11px] text-faint tabular-nums">
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
