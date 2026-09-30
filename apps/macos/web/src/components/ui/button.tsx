import { Button as BaseButton } from "@base-ui/react/button";
import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

/**
 * The one button. Callers pick `variant` and `size`; layout classes
 * (`w-full`, `justify-start`, `ml-auto`) are the only classes they may add.
 */
export const buttonVariants = cva(
	[
		"inline-flex shrink-0 select-none items-center justify-center gap-2 whitespace-nowrap",
		"rounded-control border border-transparent font-medium text-[13px] leading-none",
		"outline-none transition-[background-color,border-color,box-shadow,color,transform,opacity]",
		"duration-(--duration-functional) ease-standard",
		"focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-1 focus-visible:ring-offset-background",
		"active:scale-[0.97] active:duration-(--duration-press-in)",
		"disabled:pointer-events-none disabled:opacity-50",
		"[&_svg]:size-4 [&_svg]:shrink-0 [&_svg]:stroke-[1.75]",
	],
	{
		variants: {
			variant: {
				primary:
					"border-primary bg-primary text-primary-fg shadow-[inset_0_1px_rgb(255_255_255/16%),var(--shadow-xs)] hover:bg-primary/90",
				outline: [
					"border-input bg-popover text-foreground shadow-[0_1px_rgb(0_0_0/4%)] hover:bg-accent",
					"dark:bg-[rgb(255_255_255/3%)] dark:shadow-[0_-1px_rgb(255_255_255/6%)] dark:hover:bg-[rgb(255_255_255/6%)]",
				],
				ghost: "text-muted-foreground hover:bg-accent hover:text-foreground",
				glass:
					"surface-glass rounded-full text-foreground shadow-[0_1px_3px_rgb(0_0_0/8%)] hover:bg-accent/80",
				destructive:
					"border-destructive bg-destructive text-primary-fg shadow-[inset_0_1px_rgb(255_255_255/16%),var(--shadow-xs)] hover:bg-destructive/90",
			},
			size: {
				sm: "h-7 px-2.5 [&_svg]:size-3.5",
				md: "h-8 px-[11px]",
				lg: "h-9 px-3 text-[13.5px]",
				icon: "size-8 p-0 [&_svg]:size-[15px]",
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
