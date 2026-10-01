import { Dialog as BaseDialog } from "@base-ui/react/dialog";
import { XIcon } from "lucide-react";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";
import { Button } from "./button";

/**
 * An in-page dialog (destructive confirmations stay native, Decision 6). The
 * popup is glass at 2xl with its own deep shadow over a blurred backdrop and
 * carries no padding of its own: `DialogHeader`, `DialogBody` and
 * `DialogFooter` lay out the content (the body wraps a `ScrollArea` when it
 * can grow). Widths come from the caller as `max-w-lg` or `max-w-xl`;
 * `max-w-md` is the default. 200 ms ease-in-out, scale from 0.98.
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
					"dialog-backdrop fixed inset-0 z-40 transition-opacity duration-(--duration-surface) ease-in-out",
					"data-ending-style:opacity-0 data-starting-style:opacity-0",
				)}
			/>
			<BaseDialog.Popup
				className={cn(
					"dialog-glass fixed top-1/2 left-1/2 z-50 flex max-h-[calc(100vh-32px)] w-full max-w-md -translate-x-1/2 -translate-y-1/2 flex-col rounded-2xl border text-foreground shadow-dialog outline-none",
					"transition-[opacity,scale] duration-(--duration-surface) ease-in-out",
					"data-starting-style:scale-[0.98] data-starting-style:opacity-0",
					"data-ending-style:scale-[0.98] data-ending-style:opacity-0",
					className,
				)}
				{...props}
			/>
		</BaseDialog.Portal>
	);
}

/** The title and description block at the top of a dialog. */
export function DialogHeader({ className, ...props }: ComponentProps<"div">) {
	return (
		<div className={cn("flex flex-col gap-2 p-6 pb-3", className)} {...props} />
	);
}

export function DialogTitle({
	className,
	...props
}: ComponentProps<typeof BaseDialog.Title> & { className?: string }) {
	return (
		<BaseDialog.Title
			className={cn("m-0 font-semibold text-xl leading-none", className)}
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
			className={cn("m-0 text-muted-foreground text-sm", className)}
			{...props}
		/>
	);
}

/** The content between header and footer; put a `ScrollArea` inside when it can grow. */
export function DialogBody({ className, ...props }: ComponentProps<"div">) {
	return (
		<div
			className={cn("flex min-h-0 flex-col gap-4 px-6 pt-3 pb-6", className)}
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

/** The ghost X in the top trailing corner of the popup. */
export function DialogCloseButton({
	className,
	"aria-label": ariaLabel = "Close",
	...props
}: DialogCloseProps) {
	return (
		<DialogClose
			render={
				<Button
					aria-label={ariaLabel}
					className={cn("absolute top-2 right-2", className)}
					size="icon-sm"
					variant="ghost"
				/>
			}
			{...props}
		>
			<XIcon aria-hidden="true" />
		</DialogClose>
	);
}

/** The action band at the foot of a dialog or a page: trailing buttons on the muted fill. */
export const footerBandClass =
	"flex items-center justify-end gap-2 border-border border-t bg-muted/72 px-6 py-4";

/** The action row at the bottom of a dialog on the muted band. */
export function DialogFooter({ className, ...props }: ComponentProps<"div">) {
	return (
		<div
			className={cn(
				footerBandClass,
				"rounded-b-[calc(var(--radius-2xl)-1px)]",
				className,
			)}
			{...props}
		/>
	);
}
