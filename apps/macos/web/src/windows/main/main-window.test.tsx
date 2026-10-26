import { fireEvent, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { AppSnapshot } from "@/bridge/contract";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { COLUMN_WIDTHS_KEY, fitColumns } from "./column-widths";
import { MainWindow } from "./main-window";

const FAILED = "00000000-0000-0000-0000-000000000044";

describe("MainWindow", () => {
	it("tells the host the page is ready once and leaves a deep link to it", async () => {
		const app = (await loadFixtureSnapshots()).app as AppSnapshot;
		const harness = await createBridgeHarness("", {
			app: { ...app, requestedMeetingID: FAILED } satisfies AppSnapshot,
		});
		renderWithBridge(<MainWindow />, harness);
		expect(screen.getByTestId("main-window")).toBeInTheDocument();
		expect(callsTo(harness.transport, "meetings.select")).toHaveLength(0);
		expect(harness.transport.calls.map((call) => call.method)).toEqual([
			"page.ready",
		]);
	});

	describe("column dividers", () => {
		afterEach(() => window.localStorage.clear());

		const columns = () =>
			screen.getByTestId("main-window").style.gridTemplateColumns;

		it("drag, take arrow keys, reset on a double-click and remember the widths", async () => {
			renderWithBridge(<MainWindow />, await createBridgeHarness());
			const sidebar = screen.getByRole("separator", {
				name: "Resize sidebar",
			});
			expect(sidebar).toHaveAttribute("aria-valuenow", "256");

			fireEvent.pointerDown(sidebar, { button: 0, clientX: 256 });
			fireEvent.pointerMove(sidebar, { buttons: 1, clientX: 300 });
			fireEvent.pointerUp(sidebar, { clientX: 300 });
			expect(sidebar).toHaveAttribute("aria-valuenow", "300");
			expect(columns()).toBe("300px 300px minmax(0,1fr)");

			const list = screen.getByRole("separator", {
				name: "Resize meeting list",
			});
			fireEvent.keyDown(list, { key: "ArrowRight", shiftKey: true });
			// 332 would leave the reading column less than 400 of jsdom's 1024.
			expect(window.innerWidth).toBe(1024);
			expect(list).toHaveAttribute("aria-valuenow", "324");
			fireEvent.keyDown(list, { key: "Home" });
			expect(list).toHaveAttribute("aria-valuenow", "240");
			expect(
				JSON.parse(window.localStorage.getItem(COLUMN_WIDTHS_KEY) ?? ""),
			).toEqual({
				sidebar: 300,
				list: 240,
			});

			fireEvent.doubleClick(sidebar);
			expect(sidebar).toHaveAttribute("aria-valuenow", "256");
		});

		it("open at the stored widths and ignore ones out of range", async () => {
			window.localStorage.setItem(
				COLUMN_WIDTHS_KEY,
				JSON.stringify({ sidebar: 320, list: 9000 }),
			);
			renderWithBridge(<MainWindow />, await createBridgeHarness());
			expect(columns()).toBe("320px 300px minmax(0,1fr)");
		});

		it("narrow the list, then the sidebar, so the reading column keeps its room", () => {
			expect(fitColumns({ sidebar: 300, list: 400 }, 900)).toEqual({
				sidebar: 260,
				list: 240,
			});
		});
	});
});
