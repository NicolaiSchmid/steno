import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { RecordMark } from "./record-mark";

describe("RecordMark", () => {
	it("is decorative and still while idle", () => {
		const { container } = render(<RecordMark />);
		const mark = container.firstElementChild;
		expect(mark).toHaveAttribute("aria-hidden", "true");
		expect(mark).not.toHaveClass("animate-status-pulse");
		expect(mark?.querySelectorAll("i")).toHaveLength(4);
	});

	it("pulses while recording", () => {
		const { container, rerender } = render(<RecordMark pulse />);
		expect(container.firstElementChild).toHaveClass("animate-status-pulse");
		rerender(<RecordMark pulse={false} />);
		expect(container.firstElementChild).not.toHaveClass("animate-status-pulse");
	});
});
