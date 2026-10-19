import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type {
	MeetingDetailSnapshot,
	OnboardingSnapshot,
	RecordingSettingsSnapshot,
} from "@/bridge/contract";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
import { platformFor, SWIFT_MAC } from "@/lib/platform";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { MeetingDetail } from "./main/meeting-detail";
import { MeetingList } from "./main/meeting-list";
import { Sidebar } from "./main/sidebar";
import { TranscriptTab } from "./main/transcript-tab";
import { OnboardingWindow } from "./onboarding/onboarding-window";
import { GeneralSection } from "./settings/general-section";
import { RecordingSection } from "./settings/recording-section";
import { SummariesSection } from "./settings/summaries-section";

const NICOLAI = "00000000-0000-0000-0000-000000000014";

/**
 * Each platform's words and keys in the windows that name the machine, the
 * file manager, the privacy settings or a shortcut. The Mac's are the Swift
 * app's, unchanged; Windows and Linux get their own (`src/lib/platform.tsx`).
 */

async function delivered(): Promise<MeetingDetailSnapshot> {
	const detail = (await loadFixtureSnapshots())[
		"meeting.detail"
	] as MeetingDetailSnapshot;
	return {
		...detail,
		isBusy: false,
		export: {
			status: "delivered",
			message: "Obsidian (Notes) · Exported 10:02",
			canReexport: true,
			canReveal: true,
		},
	};
}

describe("the meeting list", () => {
	it("focuses the search with Ctrl+F off the Mac and ⌘F on it", async () => {
		const linux = await createBridgeHarness();
		const { unmount } = renderWithBridge(
			<MeetingList />,
			linux,
			platformFor("linux"),
		);
		fireEvent.keyDown(window, { key: "f", metaKey: true });
		expect(screen.getByTestId("search-meetings")).not.toHaveFocus();
		fireEvent.keyDown(window, { key: "f", ctrlKey: true });
		expect(screen.getByTestId("search-meetings")).toHaveFocus();
		unmount();

		const mac = await createBridgeHarness();
		renderWithBridge(<MeetingList />, mac, SWIFT_MAC);
		fireEvent.keyDown(window, { key: "f", ctrlKey: true });
		expect(screen.getByTestId("search-meetings")).not.toHaveFocus();
		fireEvent.keyDown(window, { key: "f", metaKey: true });
		expect(screen.getByTestId("search-meetings")).toHaveFocus();
	});

	it("leaves Ctrl+F to the speaker picker while it has focus", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		const detail = (await loadFixtureSnapshots())[
			"meeting.detail"
		] as MeetingDetailSnapshot;
		renderWithBridge(
			<>
				<MeetingList />
				<TranscriptTab detail={detail} />
			</>,
			harness,
			platformFor("linux"),
		);
		await user.click(screen.getByTestId(`speaker-picker-${NICOLAI}`));
		const field = await screen.findByTestId(`speaker-field-${NICOLAI}`);
		expect(field).toHaveFocus();
		fireEvent.keyDown(field, { key: "f", ctrlKey: true });
		expect(field).toHaveFocus();
		expect(screen.getByTestId("search-meetings")).not.toHaveFocus();
	});
});

describe("the meeting detail", () => {
	it.each([
		["macos", "Reveal in Finder", "Reveal export", "Reveal recording", "⇧⌘E"],
		[
			"windows",
			"Show in File Explorer",
			"Show export in File Explorer",
			"Show recording in File Explorer",
			"Ctrl+Shift+E",
		],
		[
			"linux",
			"Show in folder",
			"Show export in folder",
			"Show recording in folder",
			"Ctrl+Shift+E",
		],
	] as const)(
		"on %s shows the export with %s",
		async (os, button, showExport, showRecording, exportKeys) => {
			const user = userEvent.setup();
			const harness = await createBridgeHarness("", {
				"meeting.detail": await delivered(),
			});
			renderWithBridge(<MeetingDetail />, harness, platformFor(os));
			expect(screen.getByTestId("export-status")).toHaveTextContent(
				"Obsidian (Notes) · Exported 10:02",
			);
			expect(screen.getByTestId("reveal-export")).toHaveTextContent(button);
			await user.click(screen.getByRole("button", { name: "More actions" }));
			expect(
				await screen.findByRole("menuitem", { name: showExport }),
			).toBeInTheDocument();
			expect(
				screen.getByRole("menuitem", { name: showRecording }),
			).toBeInTheDocument();
			expect(
				screen.getByRole("menuitem", { name: /^Export again/ }),
			).toHaveTextContent(exportKeys);
		},
	);

	it("exports again with the platform's keys while the host allows it", async () => {
		const detail = await delivered();
		const harness = await createBridgeHarness("", { "meeting.detail": detail });
		renderWithBridge(<MeetingDetail />, harness, platformFor("windows"));
		fireEvent.keyDown(window, { key: "E", metaKey: true, shiftKey: true });
		expect(callsTo(harness.transport, "meeting.reexport")).toHaveLength(0);
		fireEvent.keyDown(window, { key: "E", ctrlKey: true, shiftKey: true });
		expect(callsTo(harness.transport, "meeting.reexport")).toHaveLength(1);
		act(() => {
			harness.transport.emit("meeting.detail", { ...detail, isBusy: true });
		});
		fireEvent.keyDown(window, { key: "E", ctrlKey: true, shiftKey: true });
		expect(callsTo(harness.transport, "meeting.reexport")).toHaveLength(1);
	});

	it("does not export again while the actions menu is open", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("", {
			"meeting.detail": await delivered(),
		});
		renderWithBridge(<MeetingDetail />, harness, platformFor("windows"));
		await user.click(screen.getByRole("button", { name: "More actions" }));
		const menu = await screen.findByRole("menu");
		await waitFor(() =>
			expect(menu).toContainElement(document.activeElement as HTMLElement),
		);
		fireEvent.keyDown(document.activeElement as Element, {
			key: "E",
			ctrlKey: true,
			shiftKey: true,
		});
		expect(callsTo(harness.transport, "meeting.reexport")).toHaveLength(0);
	});
});

describe("the sidebar", () => {
	it("records with Ctrl+Shift+R off the Mac, and leaves ⌘⇧R to the Mac's Record menu", async () => {
		const linux = await createBridgeHarness();
		const { unmount } = renderWithBridge(
			<Sidebar />,
			linux,
			platformFor("linux"),
		);
		fireEvent.keyDown(window, { key: "R", ctrlKey: true, shiftKey: true });
		expect(callsTo(linux.transport, "recording.toggle")).toHaveLength(1);
		unmount();

		const mac = await createBridgeHarness();
		renderWithBridge(<Sidebar />, mac, platformFor("macos"));
		fireEvent.keyDown(window, { key: "R", metaKey: true, shiftKey: true });
		expect(callsTo(mac.transport, "recording.toggle")).toHaveLength(0);
	});

	it.each([
		[
			"macos",
			"Allow it in System Settings to record.",
			"Fix in System Settings",
		],
		[
			"windows",
			"Allow it in Windows Settings to record.",
			"Fix in Windows Settings",
		],
		["linux", "Allow it in your system settings to record.", undefined],
	] as const)("on %s says %s", async (os, description, fix) => {
		const harness = await createBridgeHarness("scenario=denied");
		renderWithBridge(<Sidebar />, harness, platformFor(os));
		expect(screen.getByTestId("denied-microphone")).toHaveTextContent(
			description,
		);
		if (fix) {
			expect(screen.getByTestId("fix-microphone")).toHaveTextContent(fix);
		} else {
			expect(screen.queryByTestId("fix-microphone")).not.toBeInTheDocument();
		}
	});
});

describe("onboarding", () => {
	it.each([
		["linux", ["microphone"], "One permission"],
		["windows", ["microphone", "localNetwork"], "A few permissions"],
	] as const)("on %s with %j asks for %s", async (os, kinds, asked) => {
		const onboarding = (await loadFixtureSnapshots())
			.onboarding as OnboardingSnapshot;
		const harness = await createBridgeHarness("", {
			onboarding: {
				...onboarding,
				permissions: onboarding.permissions.filter((step) =>
					(kinds as readonly string[]).includes(step.kind),
				),
			} satisfies OnboardingSnapshot,
		});
		renderWithBridge(<OnboardingWindow />, harness, platformFor(os));
		expect(screen.getByTestId("onboarding-intro")).toHaveTextContent(
			`${asked}, then where summaries come from and where meetings go. Audio never leaves this computer.`,
		);
	});

	it.each([
		[
			"linux",
			"Your system may ask when you start a recording.",
			"permission-microphone-at-recording",
			"Not checked yet",
		],
		[
			"windows",
			"Windows decides this in its privacy settings. Steno can't check it before you record.",
			"permission-microphone-open",
			"Open Windows Settings",
		],
	] as const)(
		"on %s says the system decides an unknown permission, with no Allow and no claim it is allowed",
		async (os, sentence, control, label) => {
			const user = userEvent.setup();
			const harness = await createBridgeHarness("scenario=onboarding-unknown");
			renderWithBridge(<OnboardingWindow />, harness, platformFor(os));
			const row = screen.getByTestId("permission-microphone");
			expect(row).toHaveTextContent(
				`Steno records your side of a meeting from the microphone. Required. ${sentence}`,
			);
			expect(row).not.toHaveTextContent(/allow/i);
			expect(
				screen.queryByTestId("permission-microphone-request"),
			).not.toBeInTheDocument();
			expect(screen.getByTestId(control)).toHaveTextContent(label);
			if (os === "windows") {
				await user.click(screen.getByTestId(control));
				expect(callsTo(harness.transport, "system.openSystemSettings")).toEqual(
					[
						{
							method: "system.openSystemSettings",
							params: { kind: "microphone" },
						},
					],
				);
				// The local network row keeps its own wording and Got it.
				expect(
					screen.getByTestId("permission-localNetwork-skip"),
				).toHaveTextContent("Got it");
			}
		},
	);

	it("on the Mac says the system audio test waits for the recording, in place of its explanation", async () => {
		const harness = await createBridgeHarness(
			"scenario=onboarding-waits-for-recording",
		);
		renderWithBridge(<OnboardingWindow />, harness, SWIFT_MAC);
		const row = screen.getByTestId("permission-systemAudio");
		expect(row).toHaveTextContent(
			"Stop the recording first. The test plays a short tone, and the recording would pick it up.",
		);
		expect(row).not.toHaveTextContent("macOS asks once");
		expect(
			screen.getByTestId("permission-systemAudio-request"),
		).toHaveTextContent("Run the test recording");
		expect(screen.getByTestId("permission-microphone")).not.toHaveTextContent(
			"Stop the recording first",
		);
	});
});

describe("Settings", () => {
	it("shows the calendar row on the Mac only", async () => {
		const mac = await createBridgeHarness();
		const { unmount } = renderWithBridge(<GeneralSection />, mac, SWIFT_MAC);
		expect(screen.getByTestId("permission-calendar")).toBeInTheDocument();
		unmount();
		const linux = await createBridgeHarness();
		renderWithBridge(<GeneralSection />, linux, platformFor("linux"));
		expect(screen.queryByTestId("permission-calendar")).not.toBeInTheDocument();
	});

	it("opens Windows Settings for a denied permission and none on Linux", async () => {
		const recording = (await loadFixtureSnapshots())[
			"settings.recording"
		] as RecordingSettingsSnapshot;
		const denied = {
			"settings.recording": {
				...recording,
				permissions: [
					{ kind: "microphone", state: "denied", isRequesting: false },
				],
			} satisfies RecordingSettingsSnapshot,
		};
		const windows = await createBridgeHarness("", denied);
		const { unmount } = renderWithBridge(
			<RecordingSection />,
			windows,
			platformFor("windows"),
		);
		expect(screen.getByTestId("permission-microphone-open")).toHaveTextContent(
			"Open Windows Settings",
		);
		unmount();
		const linux = await createBridgeHarness("", denied);
		renderWithBridge(<RecordingSection />, linux, platformFor("linux"));
		expect(
			screen.queryByTestId("permission-microphone-open"),
		).not.toBeInTheDocument();
	});

	it.each([
		["summaries-key-in-keyring", "your keyring"],
		["summaries-key-in-file", "a file only you can read"],
		["summaries-connected", "a file only you can read"],
	] as const)(
		"on Linux with %s says the key is in %s",
		async (scenario, where) => {
			const harness = await createBridgeHarness(`scenario=${scenario}`);
			renderWithBridge(<SummariesSection />, harness, platformFor("linux"));
			expect(screen.getByTestId("api-key")).toHaveAttribute(
				"placeholder",
				`Saved in ${where}`,
			);
		},
	);
});
