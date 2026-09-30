import { Button as BaseButton } from "@base-ui/react/button";
import { cva, type VariantProps } from "class-variance-authority";
import { ChevronDownIcon } from "lucide-react";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";
import { Menu, type MenuProps, MenuTrigger } from "./menu";

/**
 * One control with two targets: the main action and a chevron that opens a
 * menu of alternatives (the Record control in the sidebar). The look is the
 * primary button's; the chevron half lifts slightly on hover so the split
 * shows itself when the pointer arrives.
 */
export const splitButtonVariants = cva(
	[
		"inline-flex shrink-0 select-none items-stretch overflow-hidden whitespace-nowrap",
		"rounded-control border font-medium text-[13px] leading-none",
		"transition-[background-color,border-color,box-shadow,color,transform,opacity]",
		"duration-(--duration-functional) ease-standard",
		"active:scale-[0.97] active:duration-(--duration-press-in)",
		"has-[button:disabled]:pointer-events-none has-[button:disabled]:opacity-50",
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
			},
			size: {
				md: "h-8",
				lg: "h-9 text-[13.5px]",
			},
		},
		defaultVariants: { variant: "primary", size: "md" },
	},
);

const partClass =
	"outline-none focus-visible:ring-2 focus-visible:ring-primary-fg/60 focus-visible:ring-inset";

export interface SplitButtonProps
	extends Omit<ComponentProps<typeof BaseButton>, "className">,
		VariantProps<typeof splitButtonVariants> {
	/** Layout classes for the whole control. */
	className?: string;
	/** The `MenuPopup` opened by the chevron. */
	menu: ReactNode;
	/** The chevron's accessible name. */
	menuLabel: string;
	/** Controlled open state for the menu, passed to `Menu`. */
	menuProps?: Omit<MenuProps, "children">;
	/** A `data-testid` for the chevron; the main action takes the spread one. */
	menuTestId?: string;
}

export function SplitButton({
	className,
	variant,
	size,
	menu,
	menuLabel,
	menuProps,
	menuTestId,
	children,
	type = "button",
	...props
}: SplitButtonProps) {
	return (
		<Menu {...menuProps}>
			<span className={cn(splitButtonVariants({ variant, size }), className)}>
				<BaseButton
					className={cn(
						partClass,
						"flex min-w-0 flex-1 items-center gap-2 rounded-l-control px-3 text-left",
					)}
					type={type}
					{...props}
				>
					{children}
				</BaseButton>
				<MenuTrigger
					aria-label={menuLabel}
					className={cn(
						partClass,
						"grid w-[38px] shrink-0 place-items-center rounded-r-control transition-colors duration-(--duration-functional) ease-standard hover:bg-primary-fg/10 data-popup-open:bg-primary-fg/10 [&_svg]:size-3.5",
					)}
					data-testid={menuTestId}
				>
					<ChevronDownIcon aria-hidden="true" />
				</MenuTrigger>
			</span>
			{menu}
		</Menu>
	);
}
