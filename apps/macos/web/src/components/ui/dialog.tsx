import { Dialog as BaseDialog } from "@base-ui/react/dialog";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

/**
 * An in-page dialog (destructive confirmations stay native, Decision 6). The
 * popup is glass at 2xl with the popover shadow over a blurred backdrop.
 */
export const Dialog = BaseDialog.Root;

export type DialogProps = ComponentProps<typeof BaseDialog.Root>;

export interface DialogTriggerProps
	extends Omit<ComponentProps<typeof BaseDialog.Trigger>, "className"> {
	className?: string;
}

export function DialogTrigger(props: DialogTriggerProps) {
	return <BaseDialog.Trigger {...props} />;
}

export interface DialogPopupProps
	extends Omit<ComponentProps<typeof BaseDialog.Popup>, "className"> {
	className?: string;
	container?: ComponentProps<typeof BaseDialog.Portal>["container"];
}

export function DialogPopup({
	className,
	container,
	...props
}: DialogPopupProps) {
	return (
		<BaseDialog.Portal container={container}>
			<BaseDialog.Backdrop
				className={cn(
					"dialog-backdrop fixed inset-0 z-40",
					"transition-opacity duration-(--duration-surface) ease-standard",
					"data-ending-style:opacity-0 data-starting-style:opacity-0",
				)}
			/>
			<BaseDialog.Popup
				className={cn(
					"dialog-glass fixed top-1/2 left-1/2 z-50 w-[420px] max-w-[calc(100vw-32px)] -translate-x-1/2 -translate-y-1/2 rounded-2xl border p-5 text-foreground shadow-pop outline-none",
					"transition-[opacity,transform] duration-(--duration-surface) ease-standard",
					"data-starting-style:scale-[0.98] data-starting-style:opacity-0",
					"data-ending-style:scale-[0.98] data-ending-style:opacity-0",
					className,
				)}
				{...props}
			/>
		</BaseDialog.Portal>
	);
}

export function DialogTitle({
	className,
	...props
}: ComponentProps<typeof BaseDialog.Title> & { className?: string }) {
	return (
		<BaseDialog.Title
			className={cn(
				"m-0 font-semibold text-[15px] leading-[1.3] tracking-[-0.01em]",
				className,
			)}
			{...props}
		/>
	);
}

export function DialogDescription({
	className,
	...props
}: ComponentProps<typeof BaseDialog.Description> & { className?: string }) {
	return (
		<BaseDialog.Description
			className={cn(
				"mt-1.5 mb-0 text-[13px] text-muted-foreground leading-[1.45]",
				className,
			)}
			{...props}
		/>
	);
}

export interface DialogCloseProps
	extends Omit<ComponentProps<typeof BaseDialog.Close>, "className"> {
	className?: string;
}

/** Wrap a `Button` with `render`: `<DialogClose render={<Button variant="ghost" />} />`. */
export function DialogClose(props: DialogCloseProps) {
	return <BaseDialog.Close {...props} />;
}

/** The action row at the bottom of a dialog. */
export function DialogFooter({ className, ...props }: ComponentProps<"div">) {
	return (
		<div
			className={cn("mt-5 flex items-center justify-end gap-2", className)}
			{...props}
		/>
	);
}
