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
 * header-high spacer and both onboarding pages with a 52 px top only where
 * the title bar lies over the page.
 */

const PLATFORMS = [
	["the Mac", SWIFT_MAC, true],
	["Windows", platformFor("windows"), false],
	["Linux", platformFor("linux"), false],
] as const;

/** Both onboarding pages: the scenario that opens each, and its test id. */
const ONBOARDING_PAGES = [
	["first", "scenario=onboarding-unknown", "onboarding-permissions"],
	["second", "scenario=onboarding-setup", "onboarding-setup"],
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

	it.each(
		ONBOARDING_PAGES.flatMap((page) =>
			PLATFORMS.map(
				([name, platform, inset]) =>
					[page[0], name, page[1], page[2], platform, inset] as const,
			),
		),
	)(
		"at the top of onboarding's %s page on %s",
		async (_page, _name, query, testId, platform, inset) => {
			const harness = await createBridgeHarness(query);
			renderWithBridge(<OnboardingWindow />, harness, platform);
			expect(screen.getByTestId(testId)).toHaveClass(inset ? "pt-13" : "pt-6");
		},
	);
});
