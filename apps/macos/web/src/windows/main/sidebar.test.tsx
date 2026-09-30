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

	it("counts down to the auto-stop and keeps recording on request", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=recording");
		renderWithBridge(<Sidebar />, harness);
		const notice = screen.getByTestId("auto-stop");
		expect(notice).toHaveTextContent(/Stops in 0:4[12]/);
		expect(notice).toHaveTextContent("Zoom closed.");
		await user.click(screen.getByTestId("keep-recording"));
		expect(callsTo(harness.transport, "recording.keepGoing")).toHaveLength(1);
	});

	it("shows no countdown once the recorder is idle", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(<Sidebar />, harness);
		expect(screen.queryByTestId("auto-stop")).not.toBeInTheDocument();
		expect(screen.queryByTestId("recorder-warning")).not.toBeInTheDocument();
	});

	it("names a denied permission and opens System Settings for it", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=denied");
		renderWithBridge(<Sidebar />, harness);
		expect(screen.getByTestId("denied-microphone")).toHaveTextContent(
			"Steno can't use the microphone.",
		);
		expect(screen.getByTestId("sidebar-record")).toBeInTheDocument();
		await user.click(screen.getByTestId("fix-microphone"));
		expect(callsTo(harness.transport, "system.openSystemSettings")).toEqual([
			{ method: "system.openSystemSettings", params: { kind: "microphone" } },
		]);
	});

	it("keeps the recorder's warning until it is dismissed", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("", {
			recording: {
				state: "idle",
				deniedPermissions: [],
				warning: "The microphone went quiet for a while.",
			},
		});
		renderWithBridge(<Sidebar />, harness);
		expect(screen.getByTestId("recorder-warning")).toHaveTextContent(
			"The microphone went quiet for a while.",
		);
		await user.click(screen.getByTestId("dismiss-recorder-message"));
		expect(callsTo(harness.transport, "recording.clearMessages")).toHaveLength(
			1,
		);
	});

	it("shows the error over the warning", async () => {
		const harness = await createBridgeHarness("", {
			recording: {
				state: "idle",
				deniedPermissions: [],
				warning: "A warning.",
				error: "The recording could not be saved.",
			},
		});
		renderWithBridge(<Sidebar />, harness);
		expect(screen.getByTestId("recorder-error")).toHaveTextContent(
			"The recording could not be saved.",
		);
		expect(screen.queryByTestId("recorder-warning")).not.toBeInTheDocument();
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
