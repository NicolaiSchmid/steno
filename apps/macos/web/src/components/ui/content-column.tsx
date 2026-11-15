import type { ReactNode } from "react";
import { cn } from "@/lib/cn";
import { Breadcrumb, HeaderRow } from "./header";
import { ScrollArea } from "./scroll-area";

export interface ContentColumnProps {
	/** The breadcrumb trail; the last item is the page. */
	crumbs: ReactNode[];
	/** The controls at the header row's trailing edge. */
	actions?: ReactNode;
	/** A control before the breadcrumb (the narrow window's Meetings button). */
	leading?: ReactNode;
	/** Held under the header row, outside the scroll (the narrow window's recorder bar). */
	bar?: ReactNode;
	/** `reading` (the meeting, 768 px) or `settings` (the flat sections, 720 px). */
	width?: "reading" | "settings";
	children: ReactNode;
}

/**
 * The content column of a window: the header row with the breadcrumb and
 * the actions at its trailing edge, any bar held under it, then the
 * content scrolling under them,
 * centred at `width`, its first line clear of the scroll fade.
 */
export function ContentColumn({
	crumbs,
	actions,
	leading,
	bar,
	width = "reading",
	children,
}: ContentColumnProps) {
	return (
		<>
			<HeaderRow>
				{leading}
				<Breadcrumb items={crumbs} />
				{actions}
			</HeaderRow>
			{bar}
			<ScrollArea className="flex-1" fade>
				<div
					className={cn(
						"mx-auto w-full px-6 pt-6 pb-12",
						width === "reading" ? "max-w-3xl" : "max-w-180",
					)}
				>
					{children}
				</div>
			</ScrollArea>
		</>
	);
}
