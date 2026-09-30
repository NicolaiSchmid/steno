import { Checkbox as BaseCheckbox } from "@base-ui/react/checkbox";
import { CheckIcon } from "lucide-react";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

export interface CheckboxProps
	extends Omit<ComponentProps<typeof BaseCheckbox.Root>, "className"> {
	className?: string;
}

/** A 16 px rounded box; primary with the inner top highlight when checked. */
export function Checkbox({ className, ...props }: CheckboxProps) {
	return (
		<BaseCheckbox.Root
			className={cn(
				"inline-grid size-4 shrink-0 place-items-center rounded-[5px] border-[1.5px] border-input bg-card outline-none",
				"transition-[background-color,border-color] duration-(--duration-functional) ease-standard",
				"data-checked:border-primary data-checked:bg-primary data-checked:shadow-[inset_0_1px_rgb(255_255_255/16%)]",
				"data-indeterminate:border-primary data-indeterminate:bg-primary",
				"focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-1 focus-visible:ring-offset-background",
				"data-disabled:opacity-50",
				className,
			)}
			{...props}
		>
			<BaseCheckbox.Indicator className="grid place-items-center text-primary-fg data-unchecked:hidden">
				<CheckIcon aria-hidden="true" className="size-3 stroke-[2.5]" />
			</BaseCheckbox.Indicator>
		</BaseCheckbox.Root>
	);
}
