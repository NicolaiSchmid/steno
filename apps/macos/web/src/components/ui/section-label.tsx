import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

/** The small heading over a sidebar group ("Meetings", "Tags"). */
export function SectionLabel({ className, ...props }: ComponentProps<"div">) {
	return (
		<div
			className={cn(
				"select-none px-2.5 pt-2.5 pb-1 font-medium text-[11px] text-faint tracking-[0.02em]",
				className,
			)}
			{...props}
		/>
	);
}
