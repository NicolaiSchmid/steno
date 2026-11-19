import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { DRAG_THRESHOLD, deviceSize, useClickOrDrag } from "./panel-shell";

describe("deviceSize", () => {
	it("scales CSS pixels by the device pixel ratio", () => {
		expect(deviceSize({ width: 79.6, height: 41.6 }, 1)).toEqual({
			width: 79.6,
			height: 41.6,
		});
		// WebKitGTK at 120 dpi: the window must be a quarter larger.
		expect(deviceSize({ width: 80, height: 42 }, 1.25)).toEqual({
			width: 100,
			height: 52.5,
		});
		expect(deviceSize({ width: 80, height: 42 }, 2)).toEqual({
			width: 160,
			height: 84,
		});
	});

	it("reads a missing or broken ratio as 1 and an empty box as no size", () => {
		for (const ratio of [0, -1, Number.NaN, Number.POSITIVE_INFINITY]) {
			expect(deviceSize({ width: 80, height: 42 }, ratio)).toEqual({
				width: 80,
				height: 42,
			});
		}
		expect(deviceSize({ width: 0, height: 42 }, 1)).toBeUndefined();
		expect(deviceSize({ width: 80, height: 0 }, 1)).toBeUndefined();
	});
});

function Handle({ onClick, drag }: { onClick: () => void; drag: () => void }) {
	return (
		<button type="button" {...useClickOrDrag(onClick, drag)}>
			Handle
		</button>
	);
}

describe("useClickOrDrag", () => {
	function setup() {
		const onClick = vi.fn();
		const drag = vi.fn();
		render(<Handle drag={drag} onClick={onClick} />);
		return { button: screen.getByRole("button"), onClick, drag };
	}

	it("clicks on a press that stays put", () => {
		const { button, onClick, drag } = setup();
		fireEvent.pointerDown(button, { button: 0, clientX: 10, clientY: 10 });
		fireEvent.pointerMove(button, { buttons: 1, clientX: 12, clientY: 11 });
		fireEvent.click(button);
		expect(drag).not.toHaveBeenCalled();
		expect(onClick).toHaveBeenCalledOnce();
	});

	it("drags on a press that travels and swallows the click after it", () => {
		const { button, onClick, drag } = setup();
		fireEvent.pointerDown(button, { button: 0, clientX: 10, clientY: 10 });
		fireEvent.pointerMove(button, {
			buttons: 1,
			clientX: 10 + DRAG_THRESHOLD,
			clientY: 10,
		});
		fireEvent.pointerMove(button, { buttons: 1, clientX: 40, clientY: 10 });
		fireEvent.click(button);
		expect(drag).toHaveBeenCalledOnce();
		expect(onClick).not.toHaveBeenCalled();
		// The next press is a click again.
		fireEvent.pointerDown(button, { button: 0, clientX: 0, clientY: 0 });
		fireEvent.click(button);
		expect(onClick).toHaveBeenCalledOnce();
	});

	it("never drags on a move without the button held", () => {
		const { button, drag } = setup();
		fireEvent.pointerDown(button, { button: 0, clientX: 10, clientY: 10 });
		fireEvent.pointerMove(button, { buttons: 0, clientX: 60, clientY: 10 });
		expect(drag).not.toHaveBeenCalled();
	});
});
