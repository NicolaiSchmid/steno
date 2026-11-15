import { act, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { ResultLine } from "./result-line";
import { notify, ToastRegion } from "./toast";

describe("ToastRegion", () => {
	it("shows a toast in a polite live region with its tone", async () => {
		render(<ToastRegion />);
		act(() => {
			notify({ title: "Exported.", description: "Obsidian · 10:02" });
		});
		const toast = await screen.findByTestId("toast");
		expect(toast).toHaveTextContent("Exported.Obsidian · 10:02");
		expect(toast).toHaveAttribute("data-tone", "success");
		expect(toast.closest("[aria-live=polite]")).not.toBeNull();
	});
});

describe("ResultLine", () => {
	it("announces a failure as an alert and anything else as a status", () => {
		const { rerender } = render(
			<ResultLine data-testid="line" tone="destructive">
				Export failed.
			</ResultLine>,
		);
		expect(screen.getByTestId("line")).toHaveAttribute("role", "alert");
		rerender(
			<ResultLine data-testid="line" tone="success">
				Exported.
			</ResultLine>,
		);
		expect(screen.getByTestId("line")).toHaveAttribute("role", "status");
	});
});
