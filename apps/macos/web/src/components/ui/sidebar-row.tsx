import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";

/**
 * A sidebar filter or tag: icon, label and a trailing count. The active row
 * is a raised card on the sidebar surface. With a `subtitle` the row grows to
 * two lines (the Settings sections: title over a one-line status).
 */
export const sidebarRowVariants = cva(
	[
		"flex w-full shrink-0 select-none items-center gap-2 rounded-control px-2.5 text-left text-[13px] text-foreground outline-none",
		"transition-[background-color,box-shadow] duration-(--duration-functional) ease-standard",
		"hover:bg-accent focus-visible:ring-2 focus-visible:ring-primary/50",
		"[&>svg]:size-4 [&>svg]:shrink-0 [&>svg]:stroke-[1.75] [&>svg]:text-muted-foreground/60",
	],
	{
		variants: {
			active: {
				true: "bg-row-selected font-medium shadow-[var(--shadow-xs),0_0_0_1px_var(--border)] hover:bg-row-selected [&>svg]:text-foreground",
				false: "",
			},
			variant: {
				default: "",
				/** A tag row: a faint "#" where the icon would be. */
				tag: "before:w-4 before:shrink-0 before:text-center before:text-faint before:content-['#']",
			},
			stacked: {
				true: "min-h-[44px] gap-2.5 py-1.5",
				false: "h-[30px]",
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
					<span className="truncate leading-[1.3]">{children}</span>
					<span className="truncate font-normal text-[11px] text-faint leading-[1.3]">
						{subtitle}
					</span>
				</span>
			) : (
				<span className="min-w-0 flex-1 truncate">{children}</span>
			)}
			{count !== undefined ? (
				<span className="text-faint text-xs tabular-nums">{count}</span>
			) : null}
		</button>
	);
}
