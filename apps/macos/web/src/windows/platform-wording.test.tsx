import { act, fireEvent, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type {
	MeetingDetailSnapshot,
	RecordingSettingsSnapshot,
} from "@/bridge/contract";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
import { type PlatformOS, platformFor } from "@/lib/platform";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { MeetingDetail } from "./main/meeting-detail";
import { MeetingList } from "./main/meeting-list";
import { Sidebar } from "./main/sidebar";
import { OnboardingWindow } from "./onboarding/onboarding-window";
import { GeneralSection } from "./settings/general-section";
import { RecordingSection } from "./settings/recording-section";
import { SettingsWindow } from "./settings/settings-window";
import { SummariesSection } from "./settings/summaries-section";
import { TranscriptionSection } from "./settings/transcription-section";

/**
 * Each platform's words and keys in the windows that name the machine, the
 * file manager, the privacy settings or a shortcut. The Mac's are the Swift
 * app's, unchanged; Windows and Linux get their own (`src/lib/platform.tsx`).
 */

/** The Tauri shell's page on `os`: it answers the Record shortcut itself. */
const on = (os: PlatformOS) => platformFor(os);
const SWIFT_MAC = platformFor("macos", { bindsRecordShortcut: false });

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
	it.each([
		["macos", "Press Record above, or ⌘⇧R. The menu bar item works too.", "⌘F"],
		[
			"windows",
			"Press Record above, or Ctrl+Shift+R. The tray icon works too.",
			"Ctrl+F",
		],
		[
			"linux",
			"Press Record above, or Ctrl+Shift+R. The tray icon works too.",
			"Ctrl+F",
		],
	] as const)("on %s says %s", async (os, body, find) => {
		const harness = await createBridgeHarness("scenario=empty");
		renderWithBridge(<MeetingList />, harness, on(os));
		expect(screen.getByTestId("meeting-list")).toHaveTextContent(body);
		expect(screen.getByText(find).tagName).toBe("KBD");
	});

	it("focuses the search with Ctrl+F off the Mac and ⌘F on it", async () => {
		const linux = await createBridgeHarness();
		const { unmount } = renderWithBridge(<MeetingList />, linux, on("linux"));
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
});

describe("the meeting detail", () => {
	it.each([
		["macos", "Show in Finder", "Reveal export", "Reveal recording", "⇧⌘E"],
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
			renderWithBridge(<MeetingDetail />, harness, on(os));
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
		renderWithBridge(<MeetingDetail />, harness, on("windows"));
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

	it("says where the audio stays while processing", async () => {
		const harness = await createBridgeHarness("scenario=processing");
		renderWithBridge(<MeetingDetail />, harness, on("linux"));
		expect(screen.getByTestId("processing-card")).toHaveTextContent(
			"Audio stays on this computer.",
		);
	});
});

describe("the sidebar", () => {
	it("records with Ctrl+Shift+R in the shell, and leaves ⌘⇧R to the Swift app's menu", async () => {
		const linux = await createBridgeHarness();
		const { unmount } = renderWithBridge(<Sidebar />, linux, on("linux"));
		fireEvent.keyDown(window, { key: "R", ctrlKey: true, shiftKey: true });
		expect(callsTo(linux.transport, "recording.toggle")).toHaveLength(1);
		unmount();

		const mac = await createBridgeHarness();
		renderWithBridge(<Sidebar />, mac, SWIFT_MAC);
		fireEvent.keyDown(window, { key: "R", metaKey: true, shiftKey: true });
		expect(callsTo(mac.transport, "recording.toggle")).toHaveLength(0);
	});

	it.each([
		[
			"macos",
			"Allow it in System Settings to record.",
			"Fix in System Settings",
		],
		["windows", "Allow it in Settings to record.", "Fix in Settings"],
		["linux", "Allow it in your system settings to record.", undefined],
	] as const)("on %s says %s", async (os, description, fix) => {
		const harness = await createBridgeHarness("scenario=denied");
		renderWithBridge(<Sidebar />, harness, on(os));
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
		["macos", "Audio never leaves this Mac."],
		["windows", "Audio never leaves this computer."],
		["linux", "Audio never leaves this computer."],
	] as const)("on %s says %s", async (os, sentence) => {
		const harness = await createBridgeHarness("scenario=onboarding-unknown");
		renderWithBridge(<OnboardingWindow />, harness, on(os));
		expect(screen.getByTestId("onboarding-intro")).toHaveTextContent(sentence);
	});

	it("explains the local network step in the platform's words", async () => {
		const harness = await createBridgeHarness("scenario=onboarding-unknown");
		const { unmount } = renderWithBridge(
			<OnboardingWindow />,
			harness,
			on("windows"),
		);
		expect(screen.getByTestId("permission-localNetwork")).toHaveTextContent(
			"Windows may ask to let Steno through the firewall when you pair the first phone. Optional.",
		);
		unmount();
		renderWithBridge(<OnboardingWindow />, harness, on("linux"));
		expect(screen.getByTestId("permission-localNetwork")).toHaveTextContent(
			"The Steno iPhone app sends recordings over your Wi-Fi. Optional.",
		);
	});

	it("offers no System Settings on Linux for a denied permission, only Check again", async () => {
		const harness = await createBridgeHarness("scenario=onboarding-denied");
		renderWithBridge(<OnboardingWindow />, harness, on("linux"));
		expect(
			screen.queryByTestId("permission-microphone-open"),
		).not.toBeInTheDocument();
		expect(screen.getByTestId("permission-microphone")).toHaveTextContent(
			"Not allowed",
		);
		expect(screen.getByTestId("permission-microphone-check")).toBeVisible();
	});
});

describe("Settings", () => {
	it.each([
		["macos", "Steno runs in the menu bar and records when you ask it to."],
		[
			"windows",
			"Steno runs in the system tray and records when you ask it to.",
		],
		["linux", "Steno runs in the background and records when you ask it to."],
	] as const)("on %s opens with %s", async (os, purpose) => {
		const harness = await createBridgeHarness();
		renderWithBridge(<SettingsWindow />, harness, on(os));
		expect(screen.getByTestId("section-purpose-general")).toHaveTextContent(
			purpose,
		);
	});

	it("shows the calendar row on the Mac only", async () => {
		const mac = await createBridgeHarness();
		const { unmount } = renderWithBridge(<GeneralSection />, mac, SWIFT_MAC);
		expect(screen.getByTestId("permission-calendar")).toBeInTheDocument();
		unmount();
		const linux = await createBridgeHarness();
		renderWithBridge(<GeneralSection />, linux, on("linux"));
		expect(screen.queryByTestId("permission-calendar")).not.toBeInTheDocument();
	});

	it("names the file manager and the machine in Recording and Transcription", async () => {
		const harness = await createBridgeHarness();
		const { unmount } = renderWithBridge(
			<RecordingSection />,
			harness,
			on("windows"),
		);
		expect(screen.getByTestId("reveal-folder")).toHaveTextContent(
			"Show in File Explorer",
		);
		expect(screen.getByTestId("section-purpose-recording")).toHaveTextContent(
			"Audio is recorded and kept on this computer only.",
		);
		unmount();
		renderWithBridge(<TranscriptionSection />, harness, on("linux"));
		expect(screen.getByText("On this computer")).toBeInTheDocument();
	});

	it("opens Windows' Settings for a denied permission and none on Linux", async () => {
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
			on("windows"),
		);
		expect(screen.getByTestId("permission-microphone-open")).toHaveTextContent(
			"Open Settings",
		);
		unmount();
		const linux = await createBridgeHarness("", denied);
		renderWithBridge(<RecordingSection />, linux, on("linux"));
		expect(
			screen.queryByTestId("permission-microphone-open"),
		).not.toBeInTheDocument();
	});

	it("says where the key and the sign-in live", async () => {
		const harness = await createBridgeHarness("scenario=codex-consent");
		const { unmount } = renderWithBridge(
			<SummariesSection />,
			harness,
			on("linux"),
		);
		expect(screen.getByTestId("codex-consent")).toHaveTextContent(
			"saved on this computer (~/.codex/auth.json)",
		);
		expect(screen.getByTestId("codex-consent")).toHaveTextContent(
			"Audio never leaves your computer.",
		);
		unmount();
		const keyed = await createBridgeHarness();
		renderWithBridge(<SummariesSection />, keyed, on("windows"));
		expect(
			screen.getByText(
				"Stored in Windows Credential Manager and sent only to the server above.",
			),
		).toBeInTheDocument();
	});
});
