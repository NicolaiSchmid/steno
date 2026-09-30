import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { Switch } from "./switch";

function Controlled({ onChange }: { onChange: (checked: boolean) => void }) {
	const [checked, setChecked] = useState(false);
	return (
		<Switch
			aria-label="Keep recordings"
			checked={checked}
			onCheckedChange={(next) => {
				setChecked(next);
				onChange(next);
			}}
		/>
	);
}

describe("Switch", () => {
	it("toggles on click and reports the new state", async () => {
		const user = userEvent.setup();
		const onChange = vi.fn();
		render(<Controlled onChange={onChange} />);
		const control = screen.getByRole("switch", { name: "Keep recordings" });

		expect(control).toHaveAttribute("aria-checked", "false");
		expect(control).not.toHaveAttribute("data-checked");

		await user.click(control);
		expect(onChange).toHaveBeenLastCalledWith(true);
		expect(control).toHaveAttribute("aria-checked", "true");
		expect(control).toHaveAttribute("data-checked");

		await user.click(control);
		expect(onChange).toHaveBeenLastCalledWith(false);
		expect(control).toHaveAttribute("aria-checked", "false");
	});

	it("toggles with the keyboard", async () => {
		const user = userEvent.setup();
		render(<Switch aria-label="Launch at login" defaultChecked={false} />);
		const control = screen.getByRole("switch", { name: "Launch at login" });
		control.focus();
		await user.keyboard(" ");
		expect(control).toHaveAttribute("aria-checked", "true");
	});

	it("ignores clicks when disabled", async () => {
		const user = userEvent.setup();
		const onChange = vi.fn();
		render(
			<Switch
				aria-label="Disabled"
				defaultChecked={false}
				disabled
				onCheckedChange={onChange}
			/>,
		);
		await user.click(screen.getByRole("switch", { name: "Disabled" }));
		expect(onChange).not.toHaveBeenCalled();
	});
});
