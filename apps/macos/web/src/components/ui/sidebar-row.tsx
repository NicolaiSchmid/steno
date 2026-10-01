import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";

/**
 * A sidebar filter or tag: icon, label and a trailing count. A 32 px row in
 * the sidebar's muted colour that fills in on hover; the active row sits on
 * the selected fill with no shadow and no ring.
 */
export const sidebarRowVariants = cva(
	[
		"flex h-8 w-full shrink-0 select-none items-center gap-2 rounded-control px-2.5 py-1.5 text-left font-medium text-sidebar-muted-foreground/80 text-sm outline-none",
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
				<span className="text-sidebar-muted-foreground/60 text-xs tabular-nums">
					{count}
				</span>
			) : null}
		</button>
	);
}
