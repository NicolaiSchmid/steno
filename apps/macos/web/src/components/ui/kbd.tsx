import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

export const kbdVariants = cva(
	"inline-flex shrink-0 items-center font-mono text-[11px] text-faint leading-none",
	{
		variants: {
			variant: {
				/** In a field: a keycap with a border on the muted surface. */
				key: "h-[18px] rounded-[5px] border border-border bg-muted px-[5px]",
				/** In a menu row: bare text at the trailing edge. */
				plain: "",
			},
		},
		defaultVariants: { variant: "key" },
	},
);

export interface KbdProps
	extends ComponentProps<"kbd">,
		VariantProps<typeof kbdVariants> {}

export function Kbd({ className, variant, ...props }: KbdProps) {
	return <kbd className={cn(kbdVariants({ variant }), className)} {...props} />;
}
