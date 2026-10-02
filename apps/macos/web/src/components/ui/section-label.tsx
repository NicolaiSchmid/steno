import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";

export interface SectionLabelProps extends ComponentProps<"div"> {
	/** Rendered after the hairline (the meeting list puts the date there). */
	trailing?: ReactNode;
}

/**
 * The heading of a sidebar group ("Meetings", "Tags"): a 32 px row with the
 * label, a hairline that takes the rest of the width, and anything trailing.
 */
export function SectionLabel({
	className,
	trailing,
	children,
	...props
}: SectionLabelProps) {
	return (
		<div
			className={cn(
				"mx-0.5 flex h-8 select-none items-center gap-2 px-2 font-medium text-sidebar-muted-foreground/60 text-xs",
				className,
			)}
			{...props}
		>
			{children}
			<span
				aria-hidden="true"
				className="h-px min-w-2 flex-1 bg-sidebar-border/60"
			/>
			{trailing}
		</div>
	);
}
