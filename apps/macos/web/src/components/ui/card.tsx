import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

/**
 * A bordered surface. `default` is the white card with the 1 px shadow;
 * `group` is the settings group (a lighter border and fill at 14 px);
 * `sidebar` is the alert surface on the sidebar (the paired iPhone).
 */
export const cardVariants = cva("", {
	variants: {
		variant: {
			default:
				"rounded-lg border border-border bg-card text-foreground shadow-xs",
			group:
				"rounded-xl border border-border/60 bg-card/40 text-foreground shadow-xs",
			sidebar:
				"rounded-lg border border-sidebar-border bg-sidebar-control-surface text-foreground",
		},
		padding: {
			none: "",
			/** A compact row card (the paired iPhone in the sidebar). */
			sm: "px-2.5 py-2",
			/** A list-row card (a task). */
			md: "px-3 py-2.5",
			lg: "p-4",
		},
		interactive: {
			true: "transition-colors duration-(--duration-functional) ease-standard hover:bg-accent",
			false: "",
		},
	},
	defaultVariants: {
		variant: "default",
		padding: "none",
		interactive: false,
	},
});

export interface CardProps
	extends ComponentProps<"div">,
		VariantProps<typeof cardVariants> {}

export function Card({
	className,
	variant,
	padding,
	interactive,
	...props
}: CardProps) {
	return (
		<div
			className={cn(cardVariants({ variant, padding, interactive }), className)}
			{...props}
		/>
	);
}
