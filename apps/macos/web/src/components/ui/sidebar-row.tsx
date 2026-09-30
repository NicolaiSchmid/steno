import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";

/**
 * A sidebar filter or tag: icon, label and a trailing count. The active row
 * is a raised card on the sidebar surface.
 */
export const sidebarRowVariants = cva(
	[
		"flex h-[30px] w-full shrink-0 select-none items-center gap-2 rounded-control px-2.5 text-left text-[13px] text-foreground outline-none",
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
		},
		defaultVariants: { active: false, variant: "default" },
	},
);

export interface SidebarRowProps
	extends ComponentProps<"button">,
		VariantProps<typeof sidebarRowVariants> {
	icon?: ReactNode;
	count?: number | string | undefined;
}

export function SidebarRow({
	className,
	active,
	variant,
	icon,
	count,
	children,
	...props
}: SidebarRowProps) {
	return (
		<button
			aria-current={active ? "true" : undefined}
			className={cn(sidebarRowVariants({ active, variant }), className)}
			type="button"
			{...props}
		>
			{icon}
			<span className="min-w-0 flex-1 truncate">{children}</span>
			{count !== undefined ? (
				<span className="text-faint text-xs tabular-nums">{count}</span>
			) : null}
		</button>
	);
}
