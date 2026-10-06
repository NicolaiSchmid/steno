import { screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { type Platform, platformFor, SWIFT_MAC } from "@/lib/platform";
import { createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { Sidebar } from "./main/sidebar";
import { OnboardingWindow } from "./onboarding/onboarding-window";
import { SettingsWindow } from "./settings/settings-window";

/**
 * The room the pages leave for the traffic lights. On the Mac (the Swift
 * app, which sets no platform, and the Tauri shell's overlay title bar) the
 * title bar lies over the page: the sidebars open with a header-high
 * spacer and onboarding with a 52 px top. Windows and Linux draw a native
 * title bar above the page, so neither is there (`TITLE_BAR_INSET` in
 * `src/lib/platform.tsx`).
 */

const PLATFORMS: readonly (readonly [string, Platform, boolean])[] = [
	["the Swift app's Mac", SWIFT_MAC, true],
	["macos", platformFor("macos"), true],
	["windows", platformFor("windows"), false],
	["linux", platformFor("linux"), false],
];

/** Whether `column` opens with the spacer under the traffic lights. */
function opensWithSpacer(column: HTMLElement): boolean {
	const first = column.firstElementChild;
	return (
		first?.getAttribute("aria-hidden") === "true" && first.matches(".h-13")
	);
}

describe("the title bar inset", () => {
	it.each(PLATFORMS)(
		"in the main sidebar on %s: %s",
		async (_, platform, inset) => {
			const harness = await createBridgeHarness();
			renderWithBridge(<Sidebar />, harness, platform);
			const column = screen.getByRole("complementary");
			expect(opensWithSpacer(column)).toBe(inset);
			const record = screen.getByTestId("sidebar-record");
			expect(column.firstElementChild?.contains(record)).toBe(!inset);
		},
	);

	it.each(PLATFORMS)(
		"in the Settings sidebar on %s: %s",
		async (_, platform, inset) => {
			const harness = await createBridgeHarness();
			renderWithBridge(<SettingsWindow />, harness, platform);
			const nav = screen.getByRole("navigation", { name: "Settings sections" });
			expect(opensWithSpacer(nav)).toBe(inset);
			const general = screen.getByTestId("settings-general");
			expect(nav.firstElementChild?.contains(general)).toBe(!inset);
		},
	);

	it.each(PLATFORMS)(
		"at the top of onboarding on %s: %s",
		async (_, platform, inset) => {
			for (const [scenario, page] of [
				["onboarding-unknown", "onboarding-permissions"],
				["onboarding-setup", "onboarding-setup"],
			] as const) {
				const harness = await createBridgeHarness(`scenario=${scenario}`);
				const { unmount } = renderWithBridge(
					<OnboardingWindow />,
					harness,
					platform,
				);
				const top = screen.getByTestId(page);
				expect(top).toHaveClass(inset ? "pt-13" : "pt-6");
				expect(top).not.toHaveClass(inset ? "pt-6" : "pt-13");
				unmount();
			}
		},
	);
});
