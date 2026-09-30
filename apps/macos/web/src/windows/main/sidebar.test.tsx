import { act, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { Sidebar } from "./sidebar";

describe("Sidebar", () => {
	it("shows the filters with their counts and the active one", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<Sidebar />, harness);
		expect(screen.getByTestId("nav-all")).toHaveAttribute(
			"aria-current",
			"true",
		);
		expect(screen.getByTestId("nav-all")).toHaveTextContent("All3");
		expect(screen.getByTestId("nav-failed")).toHaveTextContent("Failed1");
	});

	it("records meetings.setFilter when a filter is clicked", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<Sidebar />, harness);
		await user.click(screen.getByTestId("nav-ready"));
		expect(callsTo(harness.transport, "meetings.setFilter")).toEqual([
			{ method: "meetings.setFilter", params: { filter: "ready" } },
		]);
	});

	it("sets and clears the tag filter", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<Sidebar />, harness);
		await user.click(screen.getByTestId("tag-q4"));
		expect(callsTo(harness.transport, "meetings.setTagFilter")).toEqual([
			{ method: "meetings.setTagFilter", params: { tag: "q4" } },
		]);
		const list = (await loadFixtureSnapshots())["meetings.list"] as object;
		act(() => {
			harness.transport.emit("meetings.list", { ...list, tagFilter: "q4" });
		});
		expect(screen.getByTestId("tag-q4")).toHaveAttribute(
			"aria-current",
			"true",
		);
		await user.click(screen.getByTestId("tag-q4"));
		expect(callsTo(harness.transport, "meetings.setTagFilter")[1]).toEqual({
			method: "meetings.setTagFilter",
			params: {},
		});
	});

	it("starts a call recording from the Record control and offers in person", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<Sidebar />, harness);
		await user.click(screen.getByTestId("sidebar-record"));
		expect(callsTo(harness.transport, "recording.start")).toEqual([
			{ method: "recording.start", params: { mode: "call" } },
		]);
		await user.click(screen.getByTestId("sidebar-record-menu"));
		await user.click(await screen.findByTestId("sidebar-record-in-person"));
		expect(callsTo(harness.transport, "recording.start")).toHaveLength(2);
		expect(callsTo(harness.transport, "recording.start")[1]?.params).toEqual({
			mode: "inPerson",
		});
	});

	it("turns into Stop while recording", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=recording");
		renderWithBridge(<Sidebar />, harness);
		const stop = screen.getByTestId("sidebar-stop");
		expect(stop).toHaveTextContent(/Stop\s*12:3\d/);
		await user.click(stop);
		expect(callsTo(harness.transport, "recording.stop")).toHaveLength(1);
	});

	it("opens Settings and shows the paired iPhone", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<Sidebar />, harness);
		expect(screen.getByTestId("phone-card")).toHaveTextContent(
			"Nicolai's iPhone",
		);
		await user.click(screen.getByTestId("nav-settings"));
		expect(callsTo(harness.transport, "window.open")).toEqual([
			{ method: "window.open", params: { window: "settings" } },
		]);
	});
});
