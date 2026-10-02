import { render, screen } from "@testing-library/react";
import { SearchIcon } from "lucide-react";
import { describe, expect, it } from "vitest";
import { Button } from "./button";
import { EmptyState } from "./empty-state";

describe("EmptyState", () => {
	it.each(["md", "lg"] as const)(
		"names the block and its title by id (%s)",
		(size) => {
			render(
				<EmptyState
					action={<Button>Clear filters</Button>}
					body="Nothing matches this search."
					icon={<SearchIcon aria-hidden="true" data-testid="glyph" />}
					id="empty-meetings"
					size={size}
					title="No meetings match"
				/>,
			);
			expect(screen.getByTestId("empty-meetings")).toContainElement(
				screen.getByTestId("empty-meetings-title"),
			);
			expect(screen.getByTestId("empty-meetings-title")).toHaveTextContent(
				"No meetings match",
			);
			expect(screen.getByText("Nothing matches this search.")).toBeVisible();
			expect(screen.getByTestId("glyph")).toBeInTheDocument();
			expect(
				screen.getByRole("button", { name: "Clear filters" }),
			).toBeVisible();
		},
	);

	it("sets the title size by size", () => {
		const { rerender } = render(<EmptyState id="x" size="md" title="T" />);
		expect(screen.getByTestId("x-title")).toHaveClass("text-base");
		rerender(<EmptyState id="x" size="lg" title="T" />);
		expect(screen.getByTestId("x-title")).toHaveClass("text-xl");
	});

	it("renders without icon, body or action", () => {
		render(<EmptyState id="bare" title="Nothing here" />);
		const block = screen.getByTestId("bare");
		expect(block).toHaveTextContent(/^Nothing here$/);
		expect(block.querySelector("svg")).toBeNull();
		expect(screen.queryByRole("button")).not.toBeInTheDocument();
	});

	it("hides the fanned ghost tiles from assistive tech", () => {
		render(
			<EmptyState icon={<SearchIcon aria-hidden="true" />} id="x" title="T" />,
		);
		const ghosts = screen
			.getByTestId("x")
			.querySelectorAll("span[aria-hidden='true']:empty");
		expect(ghosts).toHaveLength(2);
	});
});
