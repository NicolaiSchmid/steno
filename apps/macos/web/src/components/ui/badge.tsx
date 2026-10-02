import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

/**
 * An 18 px label in a row's meta line: "#tag", "No summary", "Allowed".
 * Tints use the 8 percent surface with the 700-weight text (16 percent and
 * the 400 in dark); `sm` is the 16 px counter.
 */
export const badgeVariants = cva(
	"inline-flex h-4.5 min-w-4.5 shrink-0 items-center justify-center gap-1 whitespace-nowrap rounded-sm border border-transparent px-[5px] font-medium text-xs leading-none [&_svg]:size-3 [&_svg]:shrink-0 [&_svg]:opacity-80",
	{
		variants: {
			variant: {
				outline: "border-input bg-background text-foreground dark:bg-input/32",
				warning: "bg-warning-surface text-warning-foreground",
				success: "bg-success-surface text-success-foreground",
			},
			size: {
				md: "",
				sm: "h-4 min-w-4 rounded-xs px-1 text-3xs",
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
