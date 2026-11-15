import { Dialog as BaseDialog } from "@base-ui/react/dialog";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

/**
 * A panel that slides in from the window's leading edge over a dimmed
 * backdrop: the main window's sidebar and meeting list when the window is
 * too narrow to show them beside the reading column. Modal, so focus stays
 * inside and Escape or a click on the backdrop closes it. Solid sidebar
 * surface, the dialog shadow in light; the content lays itself out.
 */
export const Drawer = BaseDialog.Root;

export type DrawerProps = ComponentProps<typeof BaseDialog.Root>;

export interface DrawerPopupProps
	extends Omit<ComponentProps<typeof BaseDialog.Popup>, "className"> {
	className?: string;
}

export function DrawerPopup({ className, ...props }: DrawerPopupProps) {
	return (
		<BaseDialog.Portal>
			<BaseDialog.Backdrop
				className={cn(
					"dialog-backdrop fixed inset-0 z-40 transition-opacity duration-(--duration-surface) ease-in-out",
					"data-ending-style:opacity-0 data-starting-style:opacity-0",
				)}
			/>
			<BaseDialog.Popup
				className={cn(
					"surface-sidebar fixed inset-y-0 left-0 z-50 flex max-w-[calc(100vw-3rem)] bg-sidebar text-foreground shadow-dialog outline-none",
					"transition-transform duration-(--duration-surface) ease-drawer",
					"data-ending-style:-translate-x-full data-starting-style:-translate-x-full",
					className,
				)}
				{...props}
			/>
		</BaseDialog.Portal>
	);
}
