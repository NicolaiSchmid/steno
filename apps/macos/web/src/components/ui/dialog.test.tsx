import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { Button } from "./button";
import {
	Dialog,
	DialogBody,
	DialogClose,
	DialogCloseButton,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogPopup,
	DialogTitle,
	DialogTrigger,
} from "./dialog";

function Subject() {
	return (
		<Dialog>
			<DialogTrigger render={<Button variant="outline" />}>Open</DialogTrigger>
			<DialogPopup>
				<DialogCloseButton />
				<DialogHeader>
					<DialogTitle>Choose a vault</DialogTitle>
					<DialogDescription>Where the notes go.</DialogDescription>
				</DialogHeader>
				<DialogBody>
					<p>The body</p>
				</DialogBody>
				<DialogFooter>
					<DialogClose render={<Button variant="ghost" />}>Cancel</DialogClose>
					<Button variant="primary">Save</Button>
				</DialogFooter>
			</DialogPopup>
		</Dialog>
	);
}

describe("Dialog", () => {
	it("opens from the trigger with its title and description", async () => {
		const user = userEvent.setup();
		render(<Subject />);
		expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
		await user.click(screen.getByRole("button", { name: "Open" }));
		const dialog = await screen.findByRole("dialog", {
			name: "Choose a vault",
		});
		expect(dialog).toHaveAccessibleDescription("Where the notes go.");
		expect(screen.getByText("The body")).toBeVisible();
		expect(screen.getByRole("button", { name: "Save" })).toBeVisible();
	});

	it("closes from the X in the corner", async () => {
		const user = userEvent.setup();
		render(<Subject />);
		await user.click(screen.getByRole("button", { name: "Open" }));
		await screen.findByRole("dialog");
		await user.click(screen.getByRole("button", { name: "Close" }));
		await waitFor(() =>
			expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
		);
	});

	it("closes from a footer button wrapped in DialogClose", async () => {
		const user = userEvent.setup();
		render(<Subject />);
		await user.click(screen.getByRole("button", { name: "Open" }));
		await screen.findByRole("dialog");
		await user.click(screen.getByRole("button", { name: "Cancel" }));
		await waitFor(() =>
			expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
		);
	});

	it("lets the close button carry another label", async () => {
		const user = userEvent.setup();
		render(
			<Dialog defaultOpen>
				<DialogPopup>
					<DialogCloseButton aria-label="Dismiss" />
					<DialogHeader>
						<DialogTitle>Title</DialogTitle>
					</DialogHeader>
				</DialogPopup>
			</Dialog>,
		);
		await user.click(await screen.findByRole("button", { name: "Dismiss" }));
		await waitFor(() =>
			expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
		);
	});
});
