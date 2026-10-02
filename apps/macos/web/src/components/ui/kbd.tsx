import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

/** A key hint: a filled 20 px chip without a border, set in the UI font. */
export const kbdVariants = cva(
	"inline-flex shrink-0 select-none items-center justify-center gap-1 font-medium font-sans text-muted-foreground text-xs leading-none",
	{
		variants: {
			variant: {
				key: "h-5 min-w-5 rounded-xs bg-accent px-1",
				/** Bare text at the trailing edge of a menu row. */
				plain: "tracking-widest",
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
