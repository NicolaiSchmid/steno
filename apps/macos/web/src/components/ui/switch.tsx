import { Switch as BaseSwitch } from "@base-ui/react/switch";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

export interface SwitchProps
	extends Omit<ComponentProps<typeof BaseSwitch.Root>, "className"> {
	className?: string;
}

/**
 * An 18 by 30 track; primary when on, the 14 px thumb slides at the
 * functional tempo and stretches while pressed.
 */
export function Switch({ className, ...props }: SwitchProps) {
	return (
		<BaseSwitch.Root
			className={cn(
				"group relative inline-flex h-[18px] w-[30px] shrink-0 items-center rounded-full p-[2px] outline-none",
				"transition-[background-color,box-shadow] duration-200 ease-standard",
				"data-checked:bg-primary data-unchecked:bg-input",
				"focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1 focus-visible:ring-offset-background",
				"data-disabled:opacity-64",
				className,
			)}
			{...props}
		>
			<BaseSwitch.Thumb className="size-[14px] rounded-full bg-background shadow-sm transition-[translate,scale] duration-(--duration-functional) ease-standard group-active:scale-x-110 data-checked:translate-x-[12px] dark:bg-foreground" />
		</BaseSwitch.Root>
	);
}
