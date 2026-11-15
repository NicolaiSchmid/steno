import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

/**
 * A bordered surface. `default` is the card in a hairline box; `sidebar`
 * is the control surface on the sidebar (the paired iPhone). Settings
 * groups are flat (`FormCard`) and draw no card.
 */
export const cardVariants = cva("border text-foreground", {
	variants: {
		variant: {
			default: "surface-card rounded-surface border-border bg-card",
			sidebar: "rounded-lg border-sidebar-border bg-sidebar-control-surface",
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
