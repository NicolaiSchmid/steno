import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";
import { fieldFrameClass } from "./input";

/**
 * The multi-line field: the input's frame around a textarea that does not
 * resize because its column scrolls. `reading` sets the notes' larger type.
 */
const textareaFrameClass = cn(
	fieldFrameClass,
	"flex w-full",
	"focus-within:border-ring focus-within:shadow-none focus-within:ring-[3px] focus-within:ring-ring/24 focus-within:before:shadow-none",
	"has-[:disabled]:opacity-64 has-[:disabled]:shadow-none",
);

const textareaVariants = cva(
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
