import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";
import { fieldFrameClass, fieldStateClass } from "./input";

/**
 * The multi-line field: the input's frame around a textarea that does not
 * resize because its column scrolls. `reading` sets the notes' larger type.
 */
const textareaFrameClass = cn(fieldFrameClass, "flex w-full", fieldStateClass);

const textareaVariants = cva(
	"block w-full min-w-0 resize-none rounded-[inherit] bg-transparent px-[11px] py-[7px] outline-none placeholder:text-faint",
	{
		variants: {
			variant: {
				default: "min-h-17.5 text-sm leading-5",
				/** The notes: the reading type. */
				reading: "text-reading",
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
