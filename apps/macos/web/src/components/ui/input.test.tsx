import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Input } from "./input";
import { Textarea } from "./textarea";

describe("Input and Textarea", () => {
	it.each([
		["Input", Input],
		["Textarea", Textarea],
	] as const)(
		"%s puts the label and disabled state on the control and the layout class on the frame",
		(_, Control) => {
			render(<Control aria-label="Notes" className="mt-3" disabled />);
			const control = screen.getByRole("textbox", { name: "Notes" });
			expect(control).toBeDisabled();
			expect(control).not.toHaveClass("mt-3");
			expect(control.parentElement).toHaveClass("mt-3");
		},
	);
});
