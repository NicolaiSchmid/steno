import { useEffect, useRef } from "react";
import { send, useBridge, usePageReady, useSnapshot } from "@/bridge/hooks";
import { PermissionsPage } from "./permissions-page";
import { SetupPage } from "./setup-page";

/**
 * The onboarding window: two pages over the `onboarding` snapshot. Tells the
 * host the page is ready once, renders the page the host says is current,
 * and asks the host to close the window once the snapshot says onboarding
 * finished (Finish, or both setup rows handled), once.
 */
export function OnboardingWindow() {
	const client = useBridge();
	const onboarding = useSnapshot("onboarding");
	usePageReady(client);

	const closed = useRef(false);
	const finished = onboarding?.finished ?? false;
	useEffect(() => {
		if (finished && !closed.current) {
			closed.current = true;
			send(client, "window.close", { window: "onboarding" });
		}
	}, [client, finished]);

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
