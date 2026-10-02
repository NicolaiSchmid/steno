import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Breadcrumb, HeaderRow } from "./header";

describe("HeaderRow", () => {
	it("is a header landmark that keeps layout classes", () => {
		render(
			<HeaderRow className="z-10" inset="sm">
				<h1>Meetings</h1>
			</HeaderRow>,
		);
		const header = screen.getByRole("banner");
		expect(header).toHaveClass("z-10");
		expect(header).toHaveClass("px-3");
		expect(within(header).getByRole("heading")).toHaveTextContent("Meetings");
	});

	it("defaults to the content inset", () => {
		render(<HeaderRow />);
		expect(screen.getByRole("banner")).toHaveClass("px-5");
	});
});

describe("Breadcrumb", () => {
	it("marks the last item as the page and hides the separators", () => {
		render(<Breadcrumb items={["Meetings", "Produktstrategie 90/10"]} />);
		const current = screen.getByRole("listitem", { current: "page" });
		expect(current).toHaveTextContent("Produktstrategie 90/10");
		const items = screen.getAllByRole("listitem");
		expect(items.map((item) => item.textContent)).toEqual([
			"Meetings",
			"Produktstrategie 90/10",
		]);
		expect(screen.getByRole("list").children).toHaveLength(3);
		const separator = screen.getByRole("list").children[1];
		expect(separator).toHaveAttribute("aria-hidden", "true");
		expect(separator).toHaveTextContent("/");
	});

	it("shows a lone item as the page with no separator", () => {
		render(<Breadcrumb items={["Meetings"]} />);
		expect(screen.getByRole("listitem", { current: "page" })).toHaveTextContent(
			"Meetings",
		);
		expect(screen.getByRole("list").children).toHaveLength(1);
	});

	it("keeps two crumbs with the same words apart", () => {
		render(<Breadcrumb items={["Settings", "Settings"]} />);
		expect(screen.getAllByRole("listitem")).toHaveLength(2);
		expect(screen.getByRole("listitem", { current: "page" })).toBeVisible();
	});
});
