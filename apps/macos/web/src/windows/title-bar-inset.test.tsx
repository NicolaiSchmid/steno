import { screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { platformFor, SWIFT_MAC } from "@/lib/platform";
import { createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { Sidebar } from "./main/sidebar";
import { OnboardingWindow } from "./onboarding/onboarding-window";
import { SettingsWindow } from "./settings/settings-window";

/**
 * The windows follow the platform's `titleBarInset` (which platform has it
 * is pinned in `src/lib/platform.test.tsx`): the sidebars open with a
 * header-high spacer and onboarding with a 52 px top only where the title
 * bar lies over the page.
 */

const PLATFORMS = [
	["the Mac", SWIFT_MAC, true],
	["Linux", platformFor("linux"), false],
] as const;

/** Whether `column` opens with the spacer under the traffic lights. */
function opensWithSpacer(column: HTMLElement): boolean {
	const first = column.firstElementChild;
	return (
		first?.getAttribute("aria-hidden") === "true" && first.matches(".h-13")
	);
}

describe("the title bar inset", () => {
	it.each(PLATFORMS)(
		"in the main sidebar on %s",
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
		"in the Settings sidebar on %s",
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
		"at the top of onboarding on %s",
		async (_, platform, inset) => {
			const harness = await createBridgeHarness("scenario=onboarding-unknown");
			renderWithBridge(<OnboardingWindow />, harness, platform);
			expect(screen.getByTestId("onboarding-permissions")).toHaveClass(
				inset ? "pt-13" : "pt-6",
			);
		},
	);
});
