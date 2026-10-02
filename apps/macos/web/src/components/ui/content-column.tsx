import type { ReactNode } from "react";
import { cn } from "@/lib/cn";
import { Breadcrumb, HeaderRow } from "./header";
import { ScrollArea } from "./scroll-area";

export interface ContentColumnProps {
	/** The breadcrumb trail; the last item is the page. */
	crumbs: ReactNode[];
	/** The controls at the header row's trailing edge. */
	actions?: ReactNode;
	/** `reading` (the meeting, 768 px) or `settings` (the form cards, 896 px). */
	width?: "reading" | "settings";
	children: ReactNode;
}

/**
 * The content column of a window: the header row with the breadcrumb and
 * the actions at its trailing edge, then the content scrolling under it,
 * centred at `width`, its first line clear of the scroll fade.
 */
export function ContentColumn({
	crumbs,
	actions,
	width = "reading",
	children,
}: ContentColumnProps) {
	return (
		<>
			<HeaderRow>
				<Breadcrumb items={crumbs} />
				{actions}
			</HeaderRow>
			<ScrollArea className="flex-1" fade>
				<div
					className={cn(
						"mx-auto w-full px-6 pt-6 pb-12",
						width === "reading" ? "max-w-3xl" : "max-w-4xl",
					)}
				>
					{children}
				</div>
			</ScrollArea>
		</>
	);
}
