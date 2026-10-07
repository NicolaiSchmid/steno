import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";

export interface SidebarColumnProps extends ComponentProps<"aside"> {
	/** `aside` (the main window) or `nav` (the Settings sections). */
	as?: "aside" | "nav";
	/** What sits at the foot of the column, pushed down under the rows. */
	footer?: ReactNode;
	/**
	 * Whether the column opens with the header-high spacer under the
	 * traffic lights (`usePlatform().titleBarInset`).
	 */
	titleBarInset: boolean;
}

/**
 * The sidebar column of a window: the grained sidebar surface with its
 * trailing hairline, a header-high spacer under the traffic lights of an
 * overlay title bar, then the rows at a 1 px gap inside 8 px of padding,
 * and the footer pinned at the bottom in the same padding.
 */
export function SidebarColumn({
	as: Tag = "aside",
	className,
	footer,
	titleBarInset,
	children,
	...props
}: SidebarColumnProps) {
	return (
		<Tag
			className={cn(
				"surface-grain flex min-h-0 flex-col border-sidebar-border border-r bg-sidebar",
				className,
			)}
			{...props}
		>
			{titleBarInset ? (
				<div aria-hidden="true" className="h-13 shrink-0" />
			) : null}
			<div className="flex flex-col gap-1 p-2">{children}</div>
			{footer ? (
				<div className="mt-auto flex flex-col gap-2 p-2">{footer}</div>
			) : null}
		</Tag>
	);
}
