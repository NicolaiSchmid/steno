import { Checkbox as BaseCheckbox } from "@base-ui/react/checkbox";
import { CheckIcon } from "lucide-react";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";
import { focusRingClass } from "./focus-ring";

export interface CheckboxProps
	extends Omit<ComponentProps<typeof BaseCheckbox.Root>, "className"> {
	className?: string;
}

/**
 * A 16 px box with a 4 px radius on the field fill inside the 3:1 outline;
 * primary with a 3-stroke check when checked.
 */
export function Checkbox({ className, ...props }: CheckboxProps) {
	return (
		<BaseCheckbox.Root
			className={cn(
				"inline-grid size-4 shrink-0 place-items-center rounded-xs border border-outline bg-field outline-none",
				"transition-[background-color,border-color,box-shadow] duration-(--duration-functional) ease-standard",
				"data-checked:border-primary data-checked:bg-primary",
				"data-indeterminate:border-primary data-indeterminate:bg-primary",
				focusRingClass,
				"data-disabled:opacity-64",
				className,
			)}
			{...props}
		>
			<BaseCheckbox.Indicator className="grid place-items-center text-primary-foreground data-unchecked:hidden">
				<CheckIcon aria-hidden="true" className="size-3 stroke-[3]" />
			</BaseCheckbox.Indicator>
		</BaseCheckbox.Root>
	);
}
