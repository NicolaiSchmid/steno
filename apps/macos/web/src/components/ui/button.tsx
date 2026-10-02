import { Button as BaseButton } from "@base-ui/react/button";
import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

/** Filled buttons: the 1 px inner highlight at rest, pressed in while active. */
const filled =
	"shadow-xs not-disabled:inset-shadow-[0_1px_rgb(255_255_255/16%)] active:inset-shadow-[0_1px_rgb(0_0_0/8%)] active:shadow-none disabled:shadow-none";

/** Ghost buttons: no border, the accent fill on hover and while a menu is open. */
const ghost = "border-transparent hover:bg-accent data-popup-open:bg-accent";

/**
 * The look of each button variant, shared with `SplitButton`. Filled
 * buttons carry a 1 px inner highlight; bordered ones draw the same
 * highlight under the border through `edge-highlight` (a top highlight in
 * dark).
 */
export const buttonLook = {
	primary: [
		filled,
		"border-primary bg-primary text-primary-foreground hover:bg-primary/90",
	],
	outline: [
		"edge-highlight border-input bg-popover text-foreground shadow-xs",
		"hover:bg-accent/50 active:shadow-none active:before:shadow-none disabled:before:shadow-none",
		"dark:bg-input/32 dark:hover:bg-input/64",
		"[&_svg:not([class*='text-'])]:text-muted-foreground",
	],
	ghost: [
		ghost,
		"text-foreground [&_svg:not([class*='text-'])]:text-muted-foreground",
	],
	"ghost-muted": [ghost, "text-muted-foreground hover:text-foreground"],
	destructive: [
		filled,
		"border-destructive bg-destructive text-primary-foreground hover:bg-destructive/90",
	],
	"warning-outline": [
		"border-warning/32 bg-warning-surface text-warning-foreground shadow-xs",
		"hover:border-warning/40 hover:bg-warning/16 dark:hover:bg-warning/24",
	],
} as const;

/**
 * The one button. Callers pick `variant` and `size`; layout classes
 * (`w-full`, `justify-start`, `ml-auto`) are the only classes they may add.
 * Heights: xs 24, sm 28, md 32, lg 36; icons are 14 px at xs, 16 px otherwise.
 */
export const buttonVariants = cva(
	[
		"inline-flex shrink-0 select-none items-center justify-center gap-2 whitespace-nowrap",
		"rounded-control border font-medium text-sm leading-none outline-none",
		"transition-[background-color,border-color,box-shadow,color,scale,opacity]",
		"duration-(--duration-functional) ease-standard",
		"focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1 focus-visible:ring-offset-background",
		"active:scale-[0.97] active:duration-(--duration-press-in)",
		"disabled:pointer-events-none disabled:opacity-64",
		"[&_svg]:pointer-events-none [&_svg]:-mx-0.5 [&_svg]:size-4 [&_svg]:shrink-0",
	],
	{
		variants: {
			variant: buttonLook,
			// x-padding is `--spacing(n)` minus the 1 px border so text aligns with borderless controls
			size: {
				xs: "h-6 gap-1 rounded-md px-[7px] text-xs [&_svg]:size-3.5",
				sm: "h-7 gap-1.5 px-[9px]",
				md: "h-8 px-[11px]",
				lg: "h-9 px-[13px]",
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
