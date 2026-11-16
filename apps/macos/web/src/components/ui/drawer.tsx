import { Dialog as BaseDialog } from "@base-ui/react/dialog";
import { PanelLeftIcon, XIcon } from "lucide-react";
import type { ComponentProps, Ref } from "react";
import { cn } from "@/lib/cn";
import { Button } from "./button";

/**
 * A panel that slides in from the window's leading edge over a dimmed
 * backdrop: the main window's sidebar and meeting list when the window is
 * too narrow to show them beside the reading column. Modal, so focus stays
 * inside and Escape, a click on the backdrop or `DrawerCloseButton` closes
 * it. Solid sidebar surface, the dialog shadow in light; the content lays
 * itself out. Opened from a button that is not its trigger, it takes
 * `finalFocus` to give the focus back to that button. The main window's
 * meetings and the Settings window's sections use it.
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

export interface DrawerCloseButtonProps
	extends Omit<ComponentProps<typeof BaseDialog.Close>, "className"> {
	className?: string;
}

/**
 * The drawer's visible Close: a ghost ✕ at the trailing edge of the first
 * 52 px header row inside it, labelled for a screen reader.
 */
export function DrawerCloseButton({
	className,
	"aria-label": ariaLabel = "Close",
	...props
}: DrawerCloseButtonProps) {
	return (
		<BaseDialog.Close
			render={
				<Button
					aria-label={ariaLabel}
					className={cn("absolute top-3 right-3", className)}
					size="icon-sm"
					variant="ghost"
				/>
			}
			{...props}
		>
			<XIcon aria-hidden="true" />
		</BaseDialog.Close>
	);
}

export interface DrawerToggleProps
	extends Omit<ComponentProps<"button">, "children" | "className" | "ref"> {
	/** What the drawer holds, for a screen reader ("Show meetings"). */
	"aria-label": string;
	/**
	 * Whether the header row starts under the traffic lights of an overlay
	 * title bar (`usePlatform().titleBarInset`): the button moves clear.
	 */
	titleBarInset: boolean;
	ref?: Ref<HTMLButtonElement>;
}

/**
 * The header row's leading button that opens a drawer in a narrow window:
 * a ghost sidebar glyph, clear of the traffic lights where the title bar
 * overlays the page. The drawer takes it as `finalFocus`.
 */
export function DrawerToggle({ titleBarInset, ...props }: DrawerToggleProps) {
	return (
		<span className={cn("flex shrink-0", titleBarInset && "pl-16")}>
			<Button size="icon-sm" variant="ghost" {...props}>
				<PanelLeftIcon aria-hidden="true" />
			</Button>
		</span>
	);
}
