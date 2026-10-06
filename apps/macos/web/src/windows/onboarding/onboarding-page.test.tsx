import { render as renderPlain, screen } from "@testing-library/react";
import type { ReactElement } from "react";
import { describe, expect, it } from "vitest";
import { Button } from "@/components/ui";
import { PlatformProvider, SWIFT_MAC } from "@/lib/platform";
import { OnboardingPage } from "./onboarding-page";

/** The page reads the platform's title bar inset; the Mac's here. */
function render(ui: ReactElement) {
	return renderPlain(
		<PlatformProvider platform={SWIFT_MAC}>{ui}</PlatformProvider>,
	);
}

describe("OnboardingPage", () => {
	it("names the step, title, intro and aside and pins the footer buttons", () => {
		render(
			<OnboardingPage
				aside="Each recording is deleted after 30 days."
				footer={
					<>
						<Button data-testid="onboarding-later" variant="ghost">
							Later
						</Button>
						<Button data-testid="onboarding-done" variant="primary">
							Continue
						</Button>
					</>
				}
				intro="Steno needs a few permissions."
				step={1}
				testId="onboarding-permissions"
				title="Welcome to Steno"
			>
				<p>The cards</p>
			</OnboardingPage>,
		);
		expect(screen.getByTestId("onboarding-step")).toHaveTextContent(
			"Step 1 of 2",
		);
		expect(screen.getByRole("heading", { level: 1 })).toBe(
			screen.getByTestId("onboarding-title"),
		);
		expect(screen.getByTestId("onboarding-title")).toHaveTextContent(
			"Welcome to Steno",
		);
		expect(screen.getByTestId("onboarding-intro")).toHaveTextContent(
			"Steno needs a few permissions.",
		);
		expect(screen.getByTestId("onboarding-retention")).toHaveTextContent(
			"Each recording is deleted after 30 days.",
		);
		expect(screen.getByTestId("onboarding-permissions")).toContainElement(
			screen.getByText("The cards"),
		);
		const footer = screen.getByRole("contentinfo");
		expect(footer).toContainElement(screen.getByTestId("onboarding-later"));
		expect(footer).toContainElement(screen.getByTestId("onboarding-done"));
		expect(footer).not.toContainElement(screen.getByText("The cards"));
	});

	it("leaves the aside out when there is none", () => {
		render(
			<OnboardingPage
				footer={<Button>Finish</Button>}
				intro="Intro"
				step={2}
				testId="onboarding-setup"
				title="Set up"
			/>,
		);
		expect(screen.getByTestId("onboarding-step")).toHaveTextContent(
			"Step 2 of 2",
		);
		expect(
			screen.queryByTestId("onboarding-retention"),
		).not.toBeInTheDocument();
	});
});
