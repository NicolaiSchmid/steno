import type { ReactNode } from "react";
import { Button } from "./button";
import {
	Dialog,
	DialogClose,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogPopup,
	DialogTitle,
} from "./dialog";

export interface ConfirmDialogProps {
	open: boolean;
	onOpenChange: (open: boolean) => void;
	/** The action as a question: "Remove Nicolai's iPhone?" */
	title: ReactNode;
	/** What it costs, in words: "It can no longer send recordings…" */
	consequence: ReactNode;
	/** The red button's words, the action itself: "Remove iPhone". */
	confirmLabel: string;
	onConfirm: () => void;
	/** Test id of the confirm button. */
	confirmTestId?: string;
}

/**
 * Asks before a destructive action the host does not ask about itself:
 * the action as the title, the consequence in words, Cancel and the action
 * on a red button. Cancel comes first and takes the focus when the dialog
 * opens, so Return does not destroy anything by accident.
 */
export function ConfirmDialog({
	open,
	onOpenChange,
	title,
	consequence,
	confirmLabel,
	onConfirm,
	confirmTestId,
}: ConfirmDialogProps) {
	return (
		<Dialog onOpenChange={onOpenChange} open={open}>
			<DialogPopup data-testid="confirm-dialog" role="alertdialog">
				<DialogHeader>
					<DialogTitle>{title}</DialogTitle>
					<DialogDescription>{consequence}</DialogDescription>
				</DialogHeader>
				<DialogFooter>
					<DialogClose
						data-testid="confirm-cancel"
						render={<Button variant="outline" />}
					>
						Cancel
					</DialogClose>
					<Button
						data-testid={confirmTestId}
						onClick={() => {
							onConfirm();
							onOpenChange(false);
						}}
						variant="destructive"
					>
						{confirmLabel}
					</Button>
				</DialogFooter>
			</DialogPopup>
		</Dialog>
	);
}
