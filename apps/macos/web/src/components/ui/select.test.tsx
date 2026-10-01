import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { Select } from "./select";

const OPTIONS = [
	{ value: "default", label: "Meeting notes" },
	{ value: "standup", label: "Standup" },
	{ value: "retired", label: "Retired", disabled: true },
] as const;

describe("Select", () => {
	it("puts the test id and label on the trigger and shows the chosen label", () => {
		render(
			<Select
				aria-label="Summary template"
				data-testid="template-select"
				options={OPTIONS}
				size="xs"
				value="default"
			/>,
		);
		const trigger = screen.getByTestId("template-select");
		expect(trigger).toBe(
			screen.getByRole("combobox", { name: "Summary template" }),
		);
		expect(trigger).toHaveTextContent("Meeting notes");
	});

	it("shows the placeholder until something is chosen", () => {
		render(
			<Select aria-label="Template" options={OPTIONS} placeholder="Pick one" />,
		);
		expect(
			screen.getByRole("combobox", { name: "Template" }),
		).toHaveTextContent("Pick one");
	});

	it("reports the picked option and skips disabled ones", async () => {
		const user = userEvent.setup();
		const onValueChange = vi.fn();
		render(
			<Select
				aria-label="Template"
				onValueChange={onValueChange}
				options={OPTIONS}
				size="xs"
				value="default"
			/>,
		);
		await user.click(screen.getByRole("combobox", { name: "Template" }));
		expect(
			await screen.findByRole("option", { name: "Retired" }),
		).toHaveAttribute("aria-disabled", "true");
		await user.click(screen.getByRole("option", { name: "Standup" }));
		expect(onValueChange).toHaveBeenCalledWith("standup");
	});

	it("does not open while disabled", async () => {
		const user = userEvent.setup();
		render(
			<Select
				aria-label="Template"
				disabled
				options={OPTIONS}
				value="default"
			/>,
		);
		const trigger = screen.getByRole("combobox", { name: "Template" });
		expect(trigger).toBeDisabled();
		await user.click(trigger);
		expect(screen.queryByRole("option")).not.toBeInTheDocument();
	});
});
