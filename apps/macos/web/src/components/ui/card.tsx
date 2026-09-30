import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

/** A 1 px border plus a 1 px 5 percent shadow on the card surface. */
export const cardVariants = cva(
	"rounded-lg border border-border bg-card text-foreground shadow-xs",
	{
		variants: {
			padding: {
				none: "",
				/** A compact row card (the paired iPhone in the sidebar). */
				sm: "px-2.5 py-[9px]",
				/** A list-row card (a task). */
				md: "px-3 py-2.5",
				lg: "p-4",
			},
			interactive: {
				true: "transition-colors duration-(--duration-functional) ease-standard hover:bg-accent",
				false: "",
			},
		},
		defaultVariants: { padding: "none", interactive: false },
	},
);

export interface CardProps
	extends ComponentProps<"div">,
		VariantProps<typeof cardVariants> {}

export function Card({ className, padding, interactive, ...props }: CardProps) {
	return (
		<div
			className={cn(cardVariants({ padding, interactive }), className)}
			{...props}
		/>
	);
}
