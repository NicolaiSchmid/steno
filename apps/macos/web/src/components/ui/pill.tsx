import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

/**
 * The control-sized (24 px) badge beside the meeting title: tags and the
 * "Confirm speaker" prompt. Renders a button when `onClick` is given, a span
 * otherwise.
 */
export const pillVariants = cva(
	[
		"relative inline-flex h-6 shrink-0 items-center gap-1.5 whitespace-nowrap rounded-control border px-[7px]",
		"font-medium text-xs leading-none outline-none",
		"transition-[background-color,color,border-color,scale] duration-(--duration-functional) ease-standard",
		"focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1 focus-visible:ring-offset-background",
		"[&_svg]:-mx-0.5 [&_svg]:size-3.5 [&_svg]:shrink-0 [&_svg]:opacity-80",
		"before:pointer-events-none before:absolute before:inset-0 before:rounded-[calc(var(--control-radius)-1px)]",
	],
	{
		variants: {
			variant: {
				default:
					"border-input bg-background text-foreground shadow-xs before:shadow-[var(--edge-highlight)] dark:bg-input/32",
				live: "border-primary/32 bg-primary/8 text-primary dark:bg-primary/16",
			},
			interactive: {
				true: "hover:bg-accent/50 active:scale-[0.97]",
				false: "",
			},
		},
		compoundVariants: [
			{
				variant: "live",
				interactive: true,
				className: "hover:bg-primary/12",
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
