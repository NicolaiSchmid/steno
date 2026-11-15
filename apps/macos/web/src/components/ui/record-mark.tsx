import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

export interface RecordMarkProps extends ComponentProps<"span"> {
	/** While recording: the live red and the duty-cycled pulse, stepped so nothing repaints continuously. */
	pulse?: boolean;
	/** On a primary fill (the sidebar's Stop): the live red measured on the primary. */
	onPrimary?: boolean;
}

/** Steno's four-bar mark, drawn in the current text colour until it records. */
export function RecordMark({
	className,
	pulse = false,
	onPrimary = false,
	...props
}: RecordMarkProps) {
	return (
		<span
			aria-hidden="true"
			className={cn(
				"grid h-[13px] shrink-0 grid-flow-col items-end gap-[2.5px] [&>i]:block [&>i]:w-[2.5px] [&>i]:rounded-[1px] [&>i]:bg-current",
				pulse && "animate-status-pulse",
				pulse && (onPrimary ? "text-live-on-primary" : "text-live"),
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
