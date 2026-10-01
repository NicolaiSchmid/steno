import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";
import { HeaderRow } from "./header";

export interface SidebarColumnProps extends ComponentProps<"aside"> {
	/** `aside` (the main window) or `nav` (the Settings sections). */
	as?: "aside" | "nav";
	/** What sits at the foot of the column, pushed down under the rows. */
	footer?: ReactNode;
}

/**
 * The sidebar column of a window: the grained sidebar surface with its
 * trailing hairline, an empty header row under the traffic lights, then the
 * rows at a 1 px gap inside 8 px of padding, and the footer pinned at the
 * bottom in the same padding.
 */
export function SidebarColumn({
	as: Tag = "aside",
	className,
	footer,
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
			<HeaderRow inset="sm" />
			<div className="flex flex-col gap-1 p-2">{children}</div>
			{footer ? (
				<div className="mt-auto flex flex-col gap-2 p-2">{footer}</div>
			) : null}
		</Tag>
	);
}
