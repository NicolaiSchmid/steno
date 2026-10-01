import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { InboxIcon } from "lucide-react";
import { describe, expect, it, vi } from "vitest";
import { SidebarRow, sidebarRowVariants } from "./sidebar-row";

describe("SidebarRow", () => {
	it("is a button with the icon, label and count, current when active", async () => {
		const user = userEvent.setup();
		const onClick = vi.fn();
		render(
			<SidebarRow
				active
				count={3}
				data-testid="nav-all"
				icon={<InboxIcon aria-hidden="true" data-testid="icon" />}
				onClick={onClick}
			>
				All
			</SidebarRow>,
		);
		const row = screen.getByRole("button", { name: /All\s*3/ });
		expect(row).toHaveAttribute("type", "button");
		expect(row).toHaveAttribute("aria-current", "true");
		expect(row).toContainElement(screen.getByTestId("icon"));
		expect(row).toHaveClass("bg-row-selected");
		await user.click(row);
		expect(onClick).toHaveBeenCalledTimes(1);
	});

	it("drops aria-current and the count when neither applies", () => {
		render(<SidebarRow>Ready</SidebarRow>);
		const row = screen.getByRole("button", { name: "Ready" });
		expect(row).not.toHaveAttribute("aria-current");
		expect(row).toHaveTextContent(/^Ready$/);
		expect(row).not.toHaveClass("bg-row-selected");
	});

	it("shows a zero count rather than hiding it", () => {
		render(<SidebarRow count={0}>Failed</SidebarRow>);
		expect(
			screen.getByRole("button", { name: /Failed\s*0/ }),
		).toHaveTextContent("Failed0");
	});

	it("draws the hash before a tag row", () => {
		render(
			<SidebarRow data-testid="tag-q4" variant="tag">
				q4
			</SidebarRow>,
		);
		expect(screen.getByTestId("tag-q4").className).toContain(
			"before:content-['#']",
		);
		expect(sidebarRowVariants({ variant: "default" })).not.toContain(
			"before:content",
		);
	});
});
