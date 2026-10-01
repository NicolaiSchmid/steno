import { Checkbox as BaseCheckbox } from "@base-ui/react/checkbox";
import { CheckIcon } from "lucide-react";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

export interface CheckboxProps
	extends Omit<ComponentProps<typeof BaseCheckbox.Root>, "className"> {
	className?: string;
}

/**
 * A 16 px box with a 4 px radius on the field colours and the edge
 * highlight at rest; primary with a 3-stroke check when checked.
 */
export function Checkbox({ className, ...props }: CheckboxProps) {
	return (
		<BaseCheckbox.Root
			className={cn(
				"edge-highlight inline-grid size-4 shrink-0 place-items-center rounded-[4px] border border-input bg-background shadow-xs outline-none",
				"transition-[background-color,border-color,box-shadow] duration-(--duration-functional) ease-standard",
				"data-checked:border-primary data-checked:bg-primary data-checked:shadow-none data-checked:before:shadow-none",
				"data-indeterminate:border-primary data-indeterminate:bg-primary",
				"focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1 focus-visible:ring-offset-background",
				"data-disabled:opacity-64",
				"dark:not-data-checked:bg-input/32",
				className,
			)}
			{...props}
		>
			<BaseCheckbox.Indicator className="grid place-items-center text-primary-fg data-unchecked:hidden">
				<CheckIcon aria-hidden="true" className="size-3 stroke-[3]" />
			</BaseCheckbox.Indicator>
		</BaseCheckbox.Root>
	);
}
