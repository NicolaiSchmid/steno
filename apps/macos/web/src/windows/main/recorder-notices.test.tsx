import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { describe, expect, it } from "vitest";
import type { RecordingSnapshot } from "@/bridge/contract";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { MeetingDetail } from "./meeting-detail";
import { NarrowLayoutContext } from "./narrow-layout";

function narrow(children: ReactNode, isNarrow = true) {
	return (
		<NarrowLayoutContext.Provider
			value={{
				narrow: isNarrow,
				openMeetings: () => {},
				closeMeetings: () => {},
			}}
		>
			{children}
		</NarrowLayoutContext.Provider>
	);
}

describe("RecorderBar", () => {
	it("holds the status, Stop and the auto-stop in a narrow window, whichever meeting is open", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=recording-elsewhere");
		renderWithBridge(narrow(<MeetingDetail />), harness);
		const bar = await screen.findByTestId("recorder-bar");
		expect(bar).toHaveTextContent("Recording");
		expect(within(bar).getByTestId("auto-stop")).toHaveTextContent(
			"Zoom closed.",
		);
		expect(screen.queryByTestId("header-stop")).not.toBeInTheDocument();
		await user.click(within(bar).getByTestId("keep-recording"));
		await user.click(within(bar).getByTestId("bar-stop"));
		expect(
			harness.transport.calls
				.map((call) => call.method)
				.filter((method) => method.startsWith("recording.")),
		).toEqual(["recording.keepGoing", "recording.stop"]);
	});

	it("keeps a recorder error in view while idle, and is gone in a wide window", async () => {
		const user = userEvent.setup();
		const idle = (await loadFixtureSnapshots()).recording as RecordingSnapshot;
		const harness = await createBridgeHarness("", {
			recording: {
				...idle,
				error: "The microphone stopped sending audio.",
			} satisfies RecordingSnapshot,
		});
		const view = renderWithBridge(narrow(<MeetingDetail />), harness);
		const bar = await screen.findByTestId("recorder-bar");
		expect(within(bar).queryByTestId("bar-stop")).not.toBeInTheDocument();
		await user.click(within(bar).getByTestId("dismiss-recorder-message"));
		expect(callsTo(harness.transport, "recording.clearMessages")).toHaveLength(
			1,
		);
		view.unmount();
		renderWithBridge(narrow(<MeetingDetail />, false), harness);
		expect(await screen.findByTestId("meeting-title")).toBeInTheDocument();
		expect(screen.queryByTestId("recorder-bar")).not.toBeInTheDocument();
	});

	it("says nothing while the recorder is idle and quiet", async () => {
		renderWithBridge(narrow(<MeetingDetail />), await createBridgeHarness());
		expect(await screen.findByTestId("meeting-title")).toBeInTheDocument();
		expect(screen.queryByTestId("recorder-bar")).not.toBeInTheDocument();
	});
});
