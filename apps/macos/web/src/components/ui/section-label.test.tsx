import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { SectionLabel } from "./section-label";

describe("SectionLabel", () => {
	it("renders the label, a hidden hairline and the trailing node in order", () => {
		render(
			<SectionLabel data-testid="label" trailing={<span>Sep 29</span>}>
				Today
			</SectionLabel>,
		);
		const label = screen.getByTestId("label");
		expect(label).toHaveTextContent("TodaySep 29");
		const [hairline] = label.querySelectorAll("[aria-hidden='true']");
		expect(hairline).toHaveClass("h-px");
		expect(hairline?.nextElementSibling).toHaveTextContent("Sep 29");
	});

	it("keeps the hairline without a trailing node", () => {
		render(<SectionLabel data-testid="label">Tags</SectionLabel>);
		const label = screen.getByTestId("label");
		expect(label).toHaveTextContent(/^Tags$/);
		expect(label.querySelectorAll("[aria-hidden='true']")).toHaveLength(1);
	});
});
