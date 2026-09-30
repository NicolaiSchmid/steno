import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { Button } from "./button";
import { Menu, MenuItem, MenuPopup, MenuSeparator, MenuTrigger } from "./menu";

function Subject({ onDelete }: { onDelete?: () => void }) {
	return (
		<Menu>
			<MenuTrigger render={<Button variant="glass" />}>More</MenuTrigger>
			<MenuPopup>
				<MenuItem shortcut="⇧⌘E">Export again</MenuItem>
				<MenuSeparator />
				<MenuItem onClick={onDelete} variant="destructive">
					Delete meeting…
				</MenuItem>
			</MenuPopup>
		</Menu>
	);
}

describe("Menu", () => {
	it("is unmounted until the trigger opens it", async () => {
		const user = userEvent.setup();
		render(<Subject />);
		expect(screen.queryByRole("menu")).not.toBeInTheDocument();

		await user.click(screen.getByRole("button", { name: "More" }));

		const menu = await screen.findByRole("menu");
		expect(menu).toBeInTheDocument();
		expect(
			screen.getByRole("menuitem", { name: /Export again/ }),
		).toHaveTextContent("⇧⌘E");
	});

	it("runs the item action and closes", async () => {
		const user = userEvent.setup();
		const onDelete = vi.fn();
		render(<Subject onDelete={onDelete} />);
		await user.click(screen.getByRole("button", { name: "More" }));
		await user.click(
			await screen.findByRole("menuitem", { name: "Delete meeting…" }),
		);
		expect(onDelete).toHaveBeenCalledTimes(1);
		await waitFor(() =>
			expect(screen.queryByRole("menu")).not.toBeInTheDocument(),
		);
	});

	it("closes on Escape and returns focus to the trigger", async () => {
		const user = userEvent.setup();
		render(<Subject />);
		const trigger = screen.getByRole("button", { name: "More" });
		await user.click(trigger);
		await screen.findByRole("menu");
		await user.keyboard("{Escape}");
		await waitFor(() =>
			expect(screen.queryByRole("menu")).not.toBeInTheDocument(),
		);
		expect(trigger).toHaveFocus();
	});
});
