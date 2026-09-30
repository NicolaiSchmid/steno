import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

/** A small label in a row's meta line: meeting kind, "No summary", "Live". */
export const badgeVariants = cva(
	"inline-flex h-5 shrink-0 items-center gap-1 whitespace-nowrap rounded-[6px] px-[7px] font-medium text-[11px] leading-none [&_svg]:size-3 [&_svg]:stroke-2",
	{
		variants: {
			variant: {
				default: "bg-accent text-muted-foreground",
				warn: "bg-warning-surface text-warning",
				live: "bg-primary-soft text-primary",
			},
		},
		defaultVariants: { variant: "default" },
	},
);

export interface BadgeProps
	extends ComponentProps<"span">,
		VariantProps<typeof badgeVariants> {}

export function Badge({ className, variant, ...props }: BadgeProps) {
	return (
		<span className={cn(badgeVariants({ variant }), className)} {...props} />
	);
}
