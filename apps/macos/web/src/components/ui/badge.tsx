import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

/**
 * An 18 px label in a row's meta line: "#tag", "No summary", "Allowed".
 * Tints use the 8 percent surface with the 700-weight text (16 percent and
 * the 400 in dark); `sm` is the 16 px counter.
 */
export const badgeVariants = cva(
	"inline-flex h-4.5 min-w-4.5 shrink-0 items-center justify-center gap-1 whitespace-nowrap rounded-sm border border-transparent px-[5px] text-caption leading-none [&_svg]:size-3 [&_svg]:shrink-0",
	{
		variants: {
			variant: {
				outline: "border-border bg-field text-foreground",
				warning: "bg-warning-surface text-warning-foreground",
				success: "bg-success-surface text-success-foreground",
				/** A voice waiting for a name ("Speaker 1"): the dashed outline, no fill. */
				dashed:
					"border-dashed-outline border-dashed bg-transparent text-foreground",
			},
			size: {
				md: "",
				sm: "h-4 min-w-4 rounded-xs px-1 text-caption",
				/** A chip in a row: 24 px, label type. */
				chip: "h-6 min-w-6 rounded-md px-[7px] text-label",
			},
		},
		defaultVariants: { variant: "outline", size: "md" },
	},
);

export interface BadgeProps
	extends ComponentProps<"span">,
		VariantProps<typeof badgeVariants> {}

export function Badge({ className, variant, size, ...props }: BadgeProps) {
	return (
		<span
			className={cn(badgeVariants({ variant, size }), className)}
			{...props}
		/>
	);
}
