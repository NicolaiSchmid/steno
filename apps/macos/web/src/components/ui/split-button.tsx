import { Button as BaseButton } from "@base-ui/react/button";
import { cva, type VariantProps } from "class-variance-authority";
import { ChevronDownIcon } from "lucide-react";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";
import { buttonLook } from "./button";
import { Menu, type MenuProps, MenuTrigger } from "./menu";

/**
 * One control with two targets: the main action and a chevron that opens a
 * menu of alternatives (the Record control in the sidebar). The look is the
 * button's; the chevron half lifts slightly on hover so the split shows
 * itself when the pointer arrives.
 */
export const splitButtonVariants = cva(
	[
		"relative inline-flex shrink-0 select-none items-stretch overflow-hidden whitespace-nowrap",
		"rounded-control border font-medium text-sm leading-none",
		"transition-[background-color,border-color,box-shadow,color,scale,opacity]",
		"duration-(--duration-functional) ease-standard",
		"active:scale-[0.97] active:duration-(--duration-press-in)",
		"has-[button:disabled]:pointer-events-none has-[button:disabled]:opacity-64",
		"[&_svg]:size-4 [&_svg]:shrink-0",
	],
	{
		variants: {
			variant: {
				primary: buttonLook.primary,
				outline: buttonLook.outline,
			},
			size: {
				md: "h-8",
				lg: "h-9",
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
						"flex min-w-0 flex-1 items-center gap-2 rounded-l-control px-[11px] text-left",
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
						"grid w-8 shrink-0 place-items-center rounded-r-control transition-colors duration-(--duration-functional) ease-standard hover:bg-primary-fg/10 data-popup-open:bg-primary-fg/10 [&_svg]:size-3.5",
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
