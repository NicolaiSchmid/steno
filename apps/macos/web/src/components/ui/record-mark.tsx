import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

/** Steno's four-bar mark, drawn in the current text colour. */
export function RecordMark({ className, ...props }: ComponentProps<"span">) {
	return (
		<span
			aria-hidden="true"
			className={cn(
				"grid h-[13px] shrink-0 grid-flow-col items-end gap-[2.5px] [&>i]:block [&>i]:w-[2.5px] [&>i]:rounded-px [&>i]:bg-current",
				className,
			)}
			{...props}
		>
			<i className="h-[5px]" />
			<i className="h-[13px]" />
			<i className="h-2 opacity-70" />
			<i className="h-2.5" />
		</span>
	);
}
