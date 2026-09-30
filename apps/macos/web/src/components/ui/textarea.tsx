import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

export type TextareaProps = ComponentProps<"textarea">;

/**
 * A multi-line field in the reading column (the notes): the input's border
 * and focus ring, reading type, no resize handle because the column scrolls.
 */
export function Textarea({ className, ...props }: TextareaProps) {
	return (
		<textarea
			className={cn(
				"block w-full min-w-0 resize-none rounded-lg border border-input bg-card px-3.5 py-3 text-[15px] text-foreground leading-[1.6]",
				"shadow-[0_1px_rgb(0_0_0/4%)] outline-none transition-[border-color,box-shadow] duration-(--duration-functional) ease-standard",
				"placeholder:text-faint",
				"focus:border-primary/60 focus:ring-2 focus:ring-primary/25",
				"disabled:opacity-50",
				"dark:bg-[rgb(255_255_255/3%)]",
				className,
			)}
			{...props}
		/>
	);
}
