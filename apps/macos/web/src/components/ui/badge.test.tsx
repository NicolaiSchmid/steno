import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Badge, badgeVariants } from "./badge";

describe("Badge", () => {
	it("renders its words with the default outline look", () => {
		render(<Badge data-testid="tag">#q4</Badge>);
		const badge = screen.getByTestId("tag");
		expect(badge).toHaveTextContent("#q4");
		expect(badge).toHaveClass("border-input");
	});

	it("tints by variant", () => {
		expect(badgeVariants({ variant: "warning" })).toContain(
			"bg-warning-surface",
		);
		expect(badgeVariants({ variant: "success" })).toContain(
			"text-success-foreground",
		);
		expect(badgeVariants({ variant: "outline" })).toContain("bg-background");
	});

	it("shrinks to the 16 px counter in sm and merges the height", () => {
		const { rerender } = render(<Badge data-testid="b">1</Badge>);
		expect(screen.getByTestId("b")).toHaveClass("h-4.5");
		rerender(
			<Badge data-testid="b" size="sm">
				1
			</Badge>,
		);
		expect(screen.getByTestId("b")).toHaveClass("h-4");
		expect(screen.getByTestId("b")).not.toHaveClass("h-4.5");
	});

	it("keeps caller layout classes on the element", () => {
		render(
			<Badge className="ml-auto" size="sm" variant="warning">
				No summary
			</Badge>,
		);
		const badge = screen.getByText("No summary");
		expect(badge).toHaveClass("ml-auto");
		expect(badge).toHaveClass("text-warning-foreground");
		expect(badge).toHaveClass("text-3xs");
	});
});
