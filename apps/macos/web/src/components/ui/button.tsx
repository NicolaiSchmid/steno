import { Button as BaseButton } from "@base-ui/react/button";
import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";
import { focusRingClass } from "./focus-ring";

/** Ghost buttons: no border, the accent fill on hover and while a menu is open. */
const ghost = "border-transparent hover:bg-accent data-popup-open:bg-accent";

/**
 * The look of each button variant, shared with `SplitButton`: a fill or an
 * outline, and nothing else. No shadow, highlight or press scale; hover
 * changes the fill.
 */
export const buttonLook = {
	primary: [
		"border-primary bg-primary text-primary-foreground hover:bg-primary-hover",
		// Disabled, a step on the canvas with muted words, not a faded primary.
		"disabled:border-primary-disabled disabled:bg-primary-disabled disabled:text-primary-disabled-foreground disabled:opacity-100",
	],
	outline: [
		"border-outline bg-control text-foreground hover:bg-control-hover",
		"[&_svg:not([class*='text-'])]:text-muted-foreground",
	],
	ghost: [
		ghost,
		"text-foreground [&_svg:not([class*='text-'])]:text-muted-foreground",
	],
	"ghost-muted": [ghost, "text-muted-foreground hover:text-foreground"],
	destructive: [
		"border-destructive bg-destructive text-on-destructive hover:bg-destructive-hover",
	],
	/** An invitation to fill something in ("Name speakers"): a dashed outline, no fill. */
	dashed: [
		"border-dashed border-dashed-outline bg-transparent text-foreground",
		"hover:border-dashed-outline-hover hover:bg-accent data-popup-open:bg-accent",
		"[&_svg:not([class*='text-'])]:text-muted-foreground",
	],
	"warning-outline": [
		"border-warning-border bg-warning-surface text-warning-foreground",
		"hover:border-warning-border-hover hover:bg-warning-surface-hover",
	],
} as const;

/**
 * The one button. Callers pick `variant` and `size`; layout classes
 * (`w-full`, `justify-start`, `ml-auto`) are the only classes they may add.
 * Heights: xs 24 (list rows, badges), sm 32, md 36; icons are 14 px at xs,
 * 16 px otherwise.
 */
export const buttonVariants = cva(
	[
		"inline-flex shrink-0 select-none items-center justify-center gap-2 whitespace-nowrap",
		"rounded-control border text-label leading-none",
		"transition-[background-color,border-color,color,opacity]",
		"duration-(--duration-functional) ease-standard",
		focusRingClass,
		"disabled:pointer-events-none disabled:opacity-64",
		"[&_svg]:pointer-events-none [&_svg]:-mx-0.5 [&_svg]:size-4 [&_svg]:shrink-0",
	],
	{
		variants: {
			variant: buttonLook,
			// x-padding is `--spacing(n)` minus the 1 px border so text aligns with borderless controls
			size: {
				xs: "h-6 gap-1 rounded-md px-[7px] text-meta [&_svg]:size-3.5",
				/** A name leading a paragraph (a transcript turn's speaker): title type in bold. */
				name: "h-6 gap-1 rounded-md px-[7px] font-semibold text-title [&_svg]:size-3.5",
				sm: "h-8 gap-1.5 px-[11px]",
				md: "h-9 px-[15px]",
				/** The meeting header's speakers pill: 28 px, fully rounded, 13/500. */
				pill: "h-7 gap-1.5 rounded-full px-[9px] font-medium text-label [&_svg]:size-3.5",
				"icon-xs": "size-6 rounded-md p-0 [&_svg]:size-3.5",
				"icon-sm": "size-7 p-0",
				icon: "size-8 p-0",
			},
		},
		defaultVariants: {
			variant: "outline",
			size: "md",
		},
	},
);

export interface ButtonProps
	extends Omit<ComponentProps<typeof BaseButton>, "className">,
		VariantProps<typeof buttonVariants> {
	className?: string;
}

export function Button({
	className,
	variant,
	size,
	type = "button",
	...props
}: ButtonProps) {
	return (
		<BaseButton
			className={cn(buttonVariants({ variant, size }), className)}
			type={type}
			{...props}
		/>
	);
}
