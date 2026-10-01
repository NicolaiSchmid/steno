import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";

/**
 * A sidebar filter or tag: icon, label and a trailing count. A 32 px row in
 * the sidebar's muted colour that fills in on hover; the active row sits on
 * the selected fill with no shadow and no ring. With a `subtitle` the row
 * grows to two lines (the Settings sections: title over a one-line status).
 */
export const sidebarRowVariants = cva(
	[
		"flex w-full shrink-0 select-none items-center gap-2 rounded-control px-2.5 text-left font-medium text-sidebar-muted-foreground/80 text-sm outline-none",
		"transition-[background-color,color] duration-(--duration-functional) ease-standard",
		"hover:bg-row-hover hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring active:bg-row-active",
		"[&>svg]:size-4 [&>svg]:shrink-0 [&>svg]:text-sidebar-icon hover:[&>svg]:text-foreground",
	],
	{
		variants: {
			active: {
				true: "bg-row-selected text-foreground hover:bg-row-selected [&>svg]:text-foreground",
				false: "",
			},
			variant: {
				default: "",
				/** A tag row: a faint "#" where the icon would be. */
				tag: "before:w-4 before:shrink-0 before:text-center before:text-sidebar-icon before:content-['#']",
			},
			stacked: {
				true: "h-12 gap-2.5 rounded-lg p-2",
				false: "h-8 py-1.5",
			},
		},
		defaultVariants: { active: false, variant: "default", stacked: false },
	},
);

export interface SidebarRowProps
	extends ComponentProps<"button">,
		Omit<VariantProps<typeof sidebarRowVariants>, "stacked"> {
	icon?: ReactNode;
	count?: number | string | undefined;
	/** A one-line status under the label; the row becomes two lines. */
	subtitle?: ReactNode;
}

export function SidebarRow({
	className,
	active,
	variant,
	icon,
	count,
	subtitle,
	children,
	...props
}: SidebarRowProps) {
	const stacked = subtitle !== undefined && subtitle !== null;
	return (
		<button
			aria-current={active ? "true" : undefined}
			className={cn(
				sidebarRowVariants({ active, variant, stacked }),
				className,
			)}
			type="button"
			{...props}
		>
			{icon}
			{stacked ? (
				<span className="flex min-w-0 flex-1 flex-col gap-px">
					<span className="truncate">{children}</span>
					<span className="truncate font-normal text-sidebar-muted-foreground/75 text-xs">
						{subtitle}
					</span>
				</span>
			) : (
				<span className="min-w-0 flex-1 truncate">{children}</span>
			)}
			{count !== undefined ? (
				<span className="ml-auto text-sidebar-muted-foreground/60 text-xs tabular-nums">
					{count}
				</span>
			) : null}
		</button>
	);
}
