import { Select as BaseSelect } from "@base-ui/react/select";
import { CheckIcon, ChevronDownIcon } from "lucide-react";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";
import { buttonVariants } from "./button";
import { popupSurfaceClass } from "./menu";

export interface SelectOption<V extends string = string> {
	value: V;
	label: ReactNode;
	disabled?: boolean;
}

export interface SelectProps<V extends string = string> {
	options: readonly SelectOption<V>[];
	value?: V | null;
	defaultValue?: V | null;
	onValueChange?: (value: V | null) => void;
	placeholder?: ReactNode;
	disabled?: boolean;
	name?: string;
	size?: "sm" | "md";
	/** Layout classes for the trigger only. */
	className?: string;
	container?: ComponentProps<typeof BaseSelect.Portal>["container"];
	"aria-label"?: string;
	"data-testid"?: string;
}

/**
 * A closed list of choices. The trigger looks like the outline button; the
 * popup is the same glass as the menu with the item list aligned under it.
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
				className={cn(
					buttonVariants({ variant: "outline", size }),
					"justify-between gap-2 pr-2 font-normal data-placeholder:text-faint",
					className,
				)}
				data-testid={testId}
			>
				<BaseSelect.Value
					className="min-w-0 flex-1 truncate text-left"
					placeholder={placeholder}
				/>
				<BaseSelect.Icon className="flex shrink-0 text-muted-foreground">
					<ChevronDownIcon aria-hidden="true" className="size-3.5" />
				</BaseSelect.Icon>
			</BaseSelect.Trigger>
			<BaseSelect.Portal container={container}>
				<BaseSelect.Positioner
					alignItemWithTrigger={false}
					className="z-50 outline-none"
					sideOffset={6}
				>
					<BaseSelect.Popup
						className={cn(popupSurfaceClass, "min-w-(--anchor-width)")}
					>
						<BaseSelect.List>
							{options.map((option) => (
								<BaseSelect.Item
									className={cn(
										"grid h-[30px] cursor-default select-none grid-cols-[1fr_16px] items-center gap-2 rounded-[6px] pr-1.5 pl-2 text-[13px] text-foreground outline-none",
										"data-highlighted:bg-accent data-disabled:opacity-50",
									)}
									disabled={option.disabled}
									key={option.value}
									value={option.value}
								>
									<BaseSelect.ItemText className="min-w-0 truncate">
										{option.label}
									</BaseSelect.ItemText>
									<BaseSelect.ItemIndicator className="flex text-primary">
										<CheckIcon aria-hidden="true" className="size-4 stroke-2" />
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
