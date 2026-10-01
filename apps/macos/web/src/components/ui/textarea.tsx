import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

/**
 * The multi-line field: the input's frame (10 px radius, canvas colour, the
 * edge highlight at rest, the ring on focus) around a textarea that does not
 * resize because its column scrolls. `reading` sets the notes' larger type.
 */
export const textareaFrameClass = cn(
	"relative flex w-full rounded-lg border border-input bg-background text-foreground shadow-xs",
	"outline-none transition-[border-color,box-shadow] duration-(--duration-functional) ease-standard",
	"before:pointer-events-none before:absolute before:inset-0 before:rounded-[calc(var(--radius-lg)-1px)] before:shadow-[var(--edge-highlight)]",
	"focus-within:border-ring focus-within:shadow-none focus-within:ring-[3px] focus-within:ring-ring/24 focus-within:before:shadow-none",
	"has-[textarea:disabled]:opacity-64 has-[textarea:disabled]:shadow-none",
	"dark:bg-input/32",
);

export const textareaVariants = cva(
	"block w-full min-w-0 resize-none rounded-[inherit] bg-transparent px-[11px] py-[7px] outline-none placeholder:text-faint",
	{
		variants: {
			variant: {
				default: "min-h-[70px] text-sm leading-5",
				/** The notes: reading type at 15 px. */
				reading: "text-[15px] leading-[1.6]",
			},
		},
		defaultVariants: { variant: "default" },
	},
);

export interface TextareaProps
	extends ComponentProps<"textarea">,
		VariantProps<typeof textareaVariants> {
	/** Layout classes for the frame. */
	className?: string;
}

export function Textarea({ className, variant, ...props }: TextareaProps) {
	return (
		<span className={cn(textareaFrameClass, className)}>
			<textarea className={textareaVariants({ variant })} {...props} />
		</span>
	);
}
