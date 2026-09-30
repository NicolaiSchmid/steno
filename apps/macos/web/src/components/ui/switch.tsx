import { Switch as BaseSwitch } from "@base-ui/react/switch";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

export interface SwitchProps
	extends Omit<ComponentProps<typeof BaseSwitch.Root>, "className"> {
	className?: string;
}

/** A 32 by 20 track; primary when on, the thumb slides at the functional tempo. */
export function Switch({ className, ...props }: SwitchProps) {
	return (
		<BaseSwitch.Root
			className={cn(
				"relative inline-flex h-5 w-8 shrink-0 items-center rounded-full border border-transparent bg-input p-px outline-none",
				"transition-colors duration-(--duration-functional) ease-standard",
				"data-checked:bg-primary data-checked:shadow-[inset_0_1px_rgb(255_255_255/16%)]",
				"focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-1 focus-visible:ring-offset-background",
				"data-disabled:opacity-50",
				className,
			)}
			{...props}
		>
			<BaseSwitch.Thumb className="size-4 rounded-full bg-card shadow-[0_1px_2px_rgb(0_0_0/20%)] transition-transform duration-(--duration-functional) ease-standard data-checked:translate-x-3 dark:bg-foreground" />
		</BaseSwitch.Root>
	);
}
