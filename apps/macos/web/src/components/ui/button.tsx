import { Button as BaseButton } from "@base-ui/react/button";
import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

/**
 * The look of each button variant, shared with `SplitButton`. Filled
 * buttons carry a 1 px inner highlight; bordered ones draw the same
 * highlight under the border through `before:` (a top highlight in dark).
 */
export const buttonLook = {
	primary: [
		"border-primary bg-primary text-primary-fg shadow-xs shadow-primary/24",
		"not-disabled:inset-shadow-[0_1px_rgb(255_255_255/16%)]",
		"hover:bg-primary/90 active:inset-shadow-[0_1px_rgb(0_0_0/8%)] active:shadow-none disabled:shadow-none",
	],
	secondary:
		"border-transparent bg-secondary text-foreground hover:bg-secondary/90 active:bg-secondary/80",
	outline: [
		"border-input bg-popover text-foreground shadow-xs",
		"not-disabled:not-active:before:shadow-[var(--edge-highlight)]",
		"hover:bg-accent/50 active:shadow-none dark:bg-input/32 dark:hover:bg-input/64",
		"[&_svg:not([class*='text-'])]:text-muted-foreground",
	],
	ghost: [
		"border-transparent text-foreground hover:bg-accent data-popup-open:bg-accent",
		"[&_svg:not([class*='text-'])]:text-muted-foreground",
	],
	"ghost-muted":
		"border-transparent text-muted-foreground hover:bg-accent hover:text-foreground data-popup-open:bg-accent",
	destructive: [
		"border-destructive bg-destructive text-white shadow-xs shadow-destructive/24",
		"not-disabled:inset-shadow-[0_1px_rgb(255_255_255/16%)]",
		"hover:bg-destructive/90 active:inset-shadow-[0_1px_rgb(0_0_0/8%)] active:shadow-none disabled:shadow-none",
	],
	"warning-outline": [
		"border-warning/32 bg-warning-surface text-warning-foreground shadow-xs",
		"hover:border-warning/40 hover:bg-warning/16 dark:hover:bg-warning/24",
	],
	glass: [
		"surface-glass rounded-full border-border/60 text-foreground shadow-sm before:rounded-full",
		"hover:border-border [&_svg:not([class*='text-'])]:text-muted-foreground",
	],
} as const;

/**
 * The one button. Callers pick `variant` and `size`; layout classes
 * (`w-full`, `justify-start`, `ml-auto`) are the only classes they may add.
 * Heights: xs 24, sm 28, md 32, lg 36; the icon sizes match.
 */
export const buttonVariants = cva(
	[
		"relative inline-flex shrink-0 select-none items-center justify-center gap-2 whitespace-nowrap",
		"rounded-control border font-medium text-sm leading-none outline-none",
		"transition-[background-color,border-color,box-shadow,color,scale,opacity]",
		"duration-(--duration-functional) ease-standard",
		"before:pointer-events-none before:absolute before:inset-0 before:rounded-[calc(var(--control-radius)-1px)]",
		"focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1 focus-visible:ring-offset-background",
		"active:scale-[0.97] active:duration-(--duration-press-in)",
		"disabled:pointer-events-none disabled:opacity-64",
		"[&_svg]:pointer-events-none [&_svg]:-mx-0.5 [&_svg]:size-4 [&_svg]:shrink-0",
	],
	{
		variants: {
			variant: buttonLook,
			size: {
				xs: "h-6 gap-1 rounded-md px-[7px] text-xs before:rounded-[calc(var(--radius-md)-1px)] [&_svg]:size-3.5",
				sm: "h-7 gap-1.5 px-[9px]",
				md: "h-8 px-[11px]",
				lg: "h-9 px-[13px]",
				"icon-xs":
					"size-6 rounded-md p-0 before:rounded-[calc(var(--radius-md)-1px)] [&_svg]:size-3.5",
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
