import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

/**
 * An 18 px label in a row's meta line: meeting kind, "No summary", "Live".
 * Tints use the 8 percent surface with the 700-weight text (16 percent and
 * the 400 in dark); `sm` is the 16 px counter.
 */
export const badgeVariants = cva(
	"inline-flex h-[18px] min-w-[18px] shrink-0 items-center justify-center gap-1 whitespace-nowrap rounded-sm border border-transparent px-[5px] font-medium text-xs leading-none [&_svg]:size-3 [&_svg]:shrink-0 [&_svg]:opacity-80",
	{
		variants: {
			variant: {
				outline: "border-input bg-background text-foreground dark:bg-input/32",
				secondary: "bg-accent text-muted-foreground",
				warning: "bg-warning-surface text-warning-foreground",
				destructive: "bg-destructive-surface text-destructive-foreground",
				success: "bg-success/8 text-success-foreground dark:bg-success/16",
				live: "bg-primary/8 text-primary dark:bg-primary/16",
			},
			size: {
				default: "",
				sm: "h-4 min-w-4 rounded-[4px] px-1 text-3xs",
			},
		},
		defaultVariants: { variant: "outline", size: "default" },
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
