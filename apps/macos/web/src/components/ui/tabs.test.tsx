import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { Tabs, TabsList, TabsPanel, TabsTab } from "./tabs";

function Subject({ onChange }: { onChange?: (value: string) => void }) {
	return (
		<Tabs
			defaultValue="summary"
			onValueChange={(value) => onChange?.(String(value))}
		>
			<TabsList>
				<TabsTab value="summary">Summary</TabsTab>
				<TabsTab count={142} value="transcript">
					Transcript
				</TabsTab>
			</TabsList>
			<TabsPanel value="summary">The summary</TabsPanel>
			<TabsPanel value="transcript">The transcript</TabsPanel>
		</Tabs>
	);
}

describe("Tabs", () => {
	it("shows the count beside the label", () => {
		render(<Subject />);
		expect(
			screen.getByRole("tab", { name: /Transcript\s*142/ }),
		).toBeInTheDocument();
	});

	it("switches the active tab and panel on click", async () => {
		const user = userEvent.setup();
		const onChange = vi.fn();
		render(<Subject onChange={onChange} />);

		expect(screen.getByRole("tab", { name: "Summary" })).toHaveAttribute(
			"aria-selected",
			"true",
		);
		expect(screen.getByText("The summary")).toBeVisible();
		expect(screen.queryByText("The transcript")).not.toBeInTheDocument();

		await user.click(screen.getByRole("tab", { name: /Transcript/ }));

		expect(onChange).toHaveBeenCalledWith("transcript");
		expect(screen.getByRole("tab", { name: /Transcript/ })).toHaveAttribute(
			"aria-selected",
			"true",
		);
		expect(screen.getByRole("tab", { name: /Transcript/ })).toHaveAttribute(
			"data-active",
		);
		expect(screen.getByText("The transcript")).toBeVisible();
	});

	it("moves with the arrow keys", async () => {
		const user = userEvent.setup();
		render(<Subject />);
		await user.click(screen.getByRole("tab", { name: "Summary" }));
		await user.keyboard("{ArrowRight}");
		expect(screen.getByRole("tab", { name: /Transcript/ })).toHaveAttribute(
			"aria-selected",
			"true",
		);
	});
});
