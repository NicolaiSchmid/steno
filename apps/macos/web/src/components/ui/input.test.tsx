import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { Input, SearchInput } from "./input";
import { Textarea } from "./textarea";

describe("Input", () => {
	it("puts the label and disabled state on the control and the layout class on the frame", () => {
		render(<Input aria-label="Vault name" className="mt-3" disabled />);
		const control = screen.getByRole("textbox", { name: "Vault name" });
		expect(control).toBeDisabled();
		expect(control).not.toHaveClass("mt-3");
		expect(control.parentElement).toHaveClass("mt-3");
	});

	it("forwards value and onChange to the control", async () => {
		const user = userEvent.setup();
		const onChange = vi.fn();
		function Subject() {
			const [value, setValue] = useState("");
			return (
				<Input
					aria-label="Tag"
					onChange={(event) => {
						setValue(event.target.value);
						onChange(event.target.value);
					}}
					value={value}
				/>
			);
		}
		render(<Subject />);
		await user.type(screen.getByRole("textbox", { name: "Tag" }), "q4");
		expect(onChange).toHaveBeenLastCalledWith("q4");
		expect(screen.getByRole("textbox", { name: "Tag" })).toHaveValue("q4");
	});
});

describe("Textarea", () => {
	it("keeps rows, value and onChange on the control inside the frame", async () => {
		const user = userEvent.setup();
		const onChange = vi.fn();
		function Subject() {
			const [value, setValue] = useState("Hello");
			return (
				<Textarea
					aria-label="Notes"
					className="flex-1"
					onChange={(event) => {
						setValue(event.target.value);
						onChange(event.target.value);
					}}
					rows={12}
					value={value}
					variant="reading"
				/>
			);
		}
		render(<Subject />);
		const control = screen.getByRole("textbox", { name: "Notes" });
		expect(control.tagName).toBe("TEXTAREA");
		expect(control).toHaveAttribute("rows", "12");
		expect(control).toHaveValue("Hello");
		expect(control.parentElement).toHaveClass("flex-1");
		await user.type(control, "!");
		expect(onChange).toHaveBeenLastCalledWith("Hello!");
	});

	it("disables the control, not just the frame", () => {
		render(<Textarea aria-label="Notes" disabled />);
		expect(screen.getByRole("textbox", { name: "Notes" })).toBeDisabled();
	});
});

describe("SearchInput", () => {
	it("is a labelled search field with the shortcut hint", () => {
		render(
			<SearchInput
				aria-label="Search meetings"
				onChange={() => {}}
				placeholder="Search meetings"
				shortcut="⌘F"
				value=""
				variant="row"
			/>,
		);
		const field = screen.getByRole("searchbox", { name: "Search meetings" });
		expect(field).toHaveAttribute("type", "search");
		expect(field).toHaveAttribute("placeholder", "Search meetings");
		expect(field).toHaveValue("");
		expect(screen.getByText("⌘F").tagName).toBe("KBD");
	});

	it("forwards value and onChange in the row variant", async () => {
		const user = userEvent.setup();
		const onChange = vi.fn();
		render(
			<SearchInput
				aria-label="Search"
				onChange={(event) => onChange(event.target.value)}
				value="in"
				variant="row"
			/>,
		);
		const field = screen.getByRole("searchbox", { name: "Search" });
		expect(field).toHaveValue("in");
		await user.type(field, "v");
		expect(onChange).toHaveBeenLastCalledWith("inv");
	});

	it("defaults to the placeholder Search and no hint", () => {
		render(<SearchInput aria-label="Search" />);
		expect(screen.getByRole("searchbox", { name: "Search" })).toHaveAttribute(
			"placeholder",
			"Search",
		);
		expect(document.querySelector("kbd")).toBeNull();
	});
});
