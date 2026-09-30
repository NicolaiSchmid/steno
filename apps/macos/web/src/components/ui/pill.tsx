import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

/**
 * A capsule beside the meeting title: tags and the "Confirm speaker" prompt.
 * Renders a button when `onClick` is given, a span otherwise.
 */
export const pillVariants = cva(
	[
		"inline-flex h-[26px] shrink-0 items-center gap-1.5 whitespace-nowrap rounded-full border px-2.5",
		"font-medium text-xs leading-none outline-none [&_svg]:size-3 [&_svg]:stroke-2",
		"transition-[background-color,color,border-color] duration-(--duration-functional) ease-standard",
		"focus-visible:ring-2 focus-visible:ring-primary/50",
	],
	{
		variants: {
			variant: {
				default:
					"border-border bg-card text-muted-foreground shadow-[0_1px_rgb(0_0_0/4%)]",
				live: "border-transparent bg-primary-soft text-primary shadow-none",
			},
			interactive: {
				true: "cursor-default hover:bg-accent active:scale-[0.97]",
				false: "",
			},
		},
		compoundVariants: [
			{
				variant: "live",
				interactive: true,
				className: "hover:bg-primary-soft hover:brightness-95",
			},
		],
		defaultVariants: { variant: "default", interactive: false },
	},
);

export interface PillProps
	extends ComponentProps<"button">,
		Omit<VariantProps<typeof pillVariants>, "interactive"> {}

export function Pill({ className, variant, onClick, ...props }: PillProps) {
	const classes = cn(
		pillVariants({ variant, interactive: Boolean(onClick) }),
		className,
	);
	if (onClick) {
		return (
			<button className={classes} onClick={onClick} type="button" {...props} />
		);
	}
	return <span className={classes} {...props} />;
}
