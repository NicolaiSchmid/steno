import { Select as BaseSelect } from "@base-ui/react/select";
import { cva } from "class-variance-authority";
import { CheckIcon, ChevronDownIcon } from "lucide-react";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";
import { popupSurfaceClass } from "./menu";

export interface SelectOption<V extends string = string> {
	value: V;
	label: ReactNode;
	disabled?: boolean;
}

/**
 * The trigger: a field at 10 px radius on the canvas colour with the 1 px
 * edge highlight at rest and the field ring on focus. Heights: xs 24,
 * sm 28, md 32.
 */
export const selectTriggerVariants = cva(
	[
		"relative inline-flex min-w-0 select-none items-center justify-between gap-2 rounded-lg border border-input bg-background text-left text-foreground text-sm shadow-xs outline-none",
		"transition-[color,box-shadow,background-color] duration-(--duration-functional) ease-standard",
		"before:pointer-events-none before:absolute before:inset-0 before:rounded-[calc(var(--radius-lg)-1px)] before:shadow-[var(--edge-highlight)]",
		"focus-visible:border-ring focus-visible:shadow-none focus-visible:ring-[3px] focus-visible:ring-ring/24",
		"data-disabled:pointer-events-none data-placeholder:text-faint data-disabled:opacity-64",
		"dark:bg-input/32 [&_svg]:pointer-events-none [&_svg]:shrink-0",
	],
	{
		variants: {
			size: {
				xs: "h-6 gap-1 rounded-md px-[7px] text-xs before:rounded-[calc(var(--radius-md)-1px)]",
				sm: "h-7 px-[9px]",
				md: "h-8 px-[11px]",
			},
		},
		defaultVariants: { size: "md" },
	},
);

export interface SelectProps<V extends string = string> {
	options: readonly SelectOption<V>[];
	value?: V | null;
	defaultValue?: V | null;
	onValueChange?: (value: V | null) => void;
	placeholder?: ReactNode;
	disabled?: boolean;
	name?: string;
	size?: "xs" | "sm" | "md";
	/** Layout classes for the trigger only. */
	className?: string;
	container?: ComponentProps<typeof BaseSelect.Portal>["container"];
	"aria-label"?: string;
	"data-testid"?: string;
}

/**
 * A closed list of choices. The trigger is a field; the popup is the same
 * glass as the menu with 28 px rows aligned under it.
 */
export function Select<V extends string = string>({
	options,
	value,
	defaultValue,
	onValueChange,
	placeholder = "Choose",
	disabled,
	name,
	size = "md",
	className,
	container,
	"aria-label": ariaLabel,
	"data-testid": testId,
}: SelectProps<V>) {
	const rootProps = {
		...(value !== undefined ? { value } : {}),
		...(defaultValue !== undefined ? { defaultValue } : {}),
		...(name !== undefined ? { name } : {}),
		...(disabled !== undefined ? { disabled } : {}),
	};
	return (
		<BaseSelect.Root<V>
			items={options.map((option) => ({
				value: option.value,
				label: option.label,
			}))}
			onValueChange={(next) => onValueChange?.(next as V | null)}
			{...rootProps}
		>
			<BaseSelect.Trigger
				aria-label={ariaLabel}
				className={cn(selectTriggerVariants({ size }), className)}
				data-testid={testId}
			>
				<BaseSelect.Value
					className="min-w-0 flex-1 truncate"
					placeholder={placeholder}
				/>
				<BaseSelect.Icon className="flex">
					<ChevronDownIcon
						aria-hidden="true"
						className="-me-1 size-3 opacity-50"
					/>
				</BaseSelect.Icon>
			</BaseSelect.Trigger>
			<BaseSelect.Portal container={container}>
				<BaseSelect.Positioner
					alignItemWithTrigger={false}
					className="z-50 outline-none"
					sideOffset={4}
				>
					<BaseSelect.Popup
						className={cn(popupSurfaceClass, "min-w-(--anchor-width)")}
					>
						<BaseSelect.List>
							{options.map((option) => (
								<BaseSelect.Item
									className={cn(
										"grid min-h-7 cursor-default select-none grid-cols-[1fr_14px] items-center gap-2 rounded-sm px-2 py-1 text-foreground text-sm outline-none",
										"data-highlighted:bg-accent data-selected:bg-foreground/8 data-disabled:opacity-64",
									)}
									disabled={option.disabled}
									key={option.value}
									value={option.value}
								>
									<BaseSelect.ItemText className="min-w-0 truncate">
										{option.label}
									</BaseSelect.ItemText>
									<BaseSelect.ItemIndicator className="flex text-foreground">
										<CheckIcon aria-hidden="true" className="size-3.5" />
									</BaseSelect.ItemIndicator>
								</BaseSelect.Item>
							))}
						</BaseSelect.List>
					</BaseSelect.Popup>
				</BaseSelect.Positioner>
			</BaseSelect.Portal>
		</BaseSelect.Root>
	);
}
