import { act, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { describe, expect, it } from "vitest";
import type {
	MeetingsListSnapshot,
	RecordingSnapshot,
} from "@/bridge/contract";
import { applyScenario, loadFixtureSnapshots } from "@/bridge/mock-transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { format } from "./format";
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

/** The `recording` snapshot `scenario=recording-elsewhere` sends. */
async function elsewhereRecording(): Promise<RecordingSnapshot> {
	return applyScenario(
		await loadFixtureSnapshots(),
		new URLSearchParams("scenario=recording-elsewhere"),
	).recording as RecordingSnapshot;
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
		expect(screen.queryByTestId("header-live")).not.toBeInTheDocument();
		await user.click(within(bar).getByTestId("keep-recording"));
		await user.click(within(bar).getByTestId("bar-stop"));
		expect(
			harness.transport.calls
				.map((call) => call.method)
				.filter((method) => method.startsWith("recording.")),
		).toEqual(["recording.keepGoing", "recording.stop"]);
		// Another meeting is open: the bar names the one that records, and
		// its title opens it.
		const title = within(bar).getByTestId("bar-open-recording");
		expect(title).toHaveTextContent("Tuesday 10:08");
		await user.click(title);
		expect(
			callsTo(harness.transport, "meetings.select").map((call) => call.params),
		).toEqual([{ meetingID: "00000000-0000-0000-0000-000000000003" }]);
	});

	it("keeps the recording's title when a search hides it from the list", async () => {
		const harness = await createBridgeHarness("scenario=recording-elsewhere");
		renderWithBridge(narrow(<MeetingDetail />), harness);
		const bar = await screen.findByTestId("recorder-bar");
		const title = within(bar).getByTestId("bar-open-recording");
		expect(title).toHaveTextContent("Tuesday 10:08");
		const list = (await loadFixtureSnapshots())[
			"meetings.list"
		] as MeetingsListSnapshot;
		act(() => {
			harness.transport.emit("meetings.list", {
				...list,
				query: "investor",
				groups: list.groups
					.map((group) => ({
						...group,
						meetings: group.meetings.filter(
							(meeting) => meeting.title === "Investor update prep",
						),
					}))
					.filter((group) => group.meetings.length > 0),
			} satisfies MeetingsListSnapshot);
		});
		expect(within(bar).getByTestId("bar-open-recording")).toHaveTextContent(
			"Tuesday 10:08",
		);
	});

	it("names the recording from the host when the list never showed it", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness(
			"scenario=recording-elsewhere&query=investor",
		);
		renderWithBridge(narrow(<MeetingDetail />), harness);
		const bar = await screen.findByTestId("recorder-bar");
		const link = within(bar).getByTestId("bar-open-recording");
		expect(link).toHaveTextContent("Tuesday 10:08");
		await user.click(link);
		expect(
			callsTo(harness.transport, "meetings.select").map((call) => call.params),
		).toEqual([{ meetingID: "00000000-0000-0000-0000-000000000003" }]);
	});

	it("follows a rename the host sends while it records", async () => {
		const harness = await createBridgeHarness("scenario=recording-elsewhere");
		renderWithBridge(narrow(<MeetingDetail />), harness);
		const bar = await screen.findByTestId("recorder-bar");
		const recording = await elsewhereRecording();
		act(() => {
			harness.transport.emit("recording", {
				...recording,
				meetingTitle: "Board prep",
			} satisfies RecordingSnapshot);
		});
		expect(within(bar).getByTestId("bar-open-recording")).toHaveTextContent(
			"Board prep",
		);
	});

	it("names an untitled recording as the list would", async () => {
		const harness = await createBridgeHarness("scenario=recording-elsewhere");
		renderWithBridge(narrow(<MeetingDetail />), harness);
		const bar = await screen.findByTestId("recorder-bar");
		const { meetingTitle: _, ...untitled } = await elsewhereRecording();
		act(() => {
			harness.transport.emit("recording", untitled);
		});
		expect(within(bar).getByTestId("bar-open-recording")).toHaveTextContent(
			format.untitled(untitled.startedAt ?? ""),
		);
	});

	it("names no meeting while the open one is the one recording", async () => {
		const harness = await createBridgeHarness("scenario=recording");
		renderWithBridge(narrow(<MeetingDetail />), harness);
		const bar = await screen.findByTestId("recorder-bar");
		expect(
			within(bar).queryByTestId("bar-open-recording"),
		).not.toBeInTheDocument();
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
