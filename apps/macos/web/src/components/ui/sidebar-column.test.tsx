import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { SidebarColumn } from "./sidebar-column";
import { SidebarRow } from "./sidebar-row";

describe("SidebarColumn", () => {
	it("is an aside by default with the rows under a header-high spacer", () => {
		render(
			<SidebarColumn data-testid="column" titleBarInset>
				<SidebarRow>All</SidebarRow>
			</SidebarColumn>,
		);
		const column = screen.getByTestId("column");
		expect(column.tagName).toBe("ASIDE");
		expect(column).toBe(screen.getByRole("complementary"));
		expect(column.firstElementChild).toHaveAttribute("aria-hidden", "true");
		expect(column.firstElementChild).toHaveClass("h-13");
		expect(screen.queryByRole("banner")).not.toBeInTheDocument();
		expect(screen.getByRole("button", { name: "All" })).toBeInTheDocument();
		expect(screen.queryByTestId("footer")).not.toBeInTheDocument();
	});

	it("renders as a labelled nav with the footer pinned at the bottom", () => {
		render(
			<SidebarColumn
				aria-label="Settings sections"
				as="nav"
				className="w-64"
				footer={<SidebarRow data-testid="footer">Settings</SidebarRow>}
				titleBarInset
			>
				<SidebarRow>General</SidebarRow>
			</SidebarColumn>,
		);
		const nav = screen.getByRole("navigation", { name: "Settings sections" });
		expect(nav.tagName).toBe("NAV");
		expect(nav).toHaveClass("w-64");
		expect(nav.lastElementChild).toContainElement(screen.getByTestId("footer"));
		expect(nav.lastElementChild).toHaveClass("mt-auto");
	});

	it("opens with the rows under a native title bar", () => {
		render(
			<SidebarColumn data-testid="column" titleBarInset={false}>
				<SidebarRow>All</SidebarRow>
			</SidebarColumn>,
		);
		const column = screen.getByTestId("column");
		expect(column.querySelector('[aria-hidden="true"].h-13')).toBeNull();
		expect(column.firstElementChild).toContainElement(
			screen.getByRole("button", { name: "All" }),
		);
	});
});
