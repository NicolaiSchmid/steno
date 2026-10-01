import { useBridge, usePageReady, useSnapshot } from "@/bridge/hooks";
import { PermissionsPage } from "./permissions-page";
import { SetupPage } from "./setup-page";

/**
 * The onboarding window: two pages over the `onboarding` snapshot. Tells the
 * host the page is ready once and renders the page the host says is
 * current; the host closes the window itself once onboarding finished.
 */
export function OnboardingWindow() {
	const client = useBridge();
	const onboarding = useSnapshot("onboarding");
	usePageReady(client);

	return (
		<div
			className="flex h-full min-h-0 flex-col overflow-hidden bg-background text-foreground"
			data-testid="onboarding-window"
		>
			{onboarding ? (
				onboarding.page === "permissions" ? (
					<PermissionsPage onboarding={onboarding} />
				) : (
					<SetupPage onboarding={onboarding} />
				)
			) : null}
		</div>
	);
}
