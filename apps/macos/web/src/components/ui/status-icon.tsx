import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

/**
 * A state glyph beside a line of text (a permission's state, where the
 * export stands): a 20 px line box so the 16 px icon centres on the text's
 * first line, coloured by `tone`.
 */
export const statusIconVariants = cva(
	"flex h-5 shrink-0 items-center [&>svg]:size-4",
	{
		variants: {
			tone: {
				muted: "text-muted-foreground",
				faint: "text-faint",
				primary: "text-primary",
				warning: "text-warning",
			},
		},
		defaultVariants: { tone: "muted" },
	},
);

export interface StatusIconProps
	extends ComponentProps<"span">,
		VariantProps<typeof statusIconVariants> {}

export function StatusIcon({ className, tone, ...props }: StatusIconProps) {
	return (
		<span className={cn(statusIconVariants({ tone }), className)} {...props} />
	);
}
