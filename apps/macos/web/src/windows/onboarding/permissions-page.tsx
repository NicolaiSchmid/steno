import type { OnboardingSnapshot } from "@/bridge/contract";
import { send, useBridge } from "@/bridge/hooks";
import { PermissionRow } from "@/components/permission-row";
import { Button, FormCard } from "@/components/ui";
import { type PlatformWords, usePlatform } from "@/lib/platform";
import { OnboardingPage } from "./onboarding-page";

/**
 * Page 1's intro, which the Mac smoke test finds page 1 by: "One
 * permission" where the platform has one (Linux), else "A few permissions".
 */
function permissionsIntro(
	{ computer }: PlatformWords,
	permissions: number,
): string {
	const asked = permissions === 1 ? "One permission" : "A few permissions";
	return `${asked}, then where summaries come from and where meetings go. Audio never leaves this ${computer}.`;
}

/**
 * Page 1: one row per permission with the action that fits its state. The
 * required ones gate Continue; the optional ones can be skipped; the local
 * network prompt comes with the first pairing, so its row only explains.
 * Later moves on with whatever is still open.
 */
export function PermissionsPage({
	onboarding,
}: {
	onboarding: OnboardingSnapshot;
}) {
	const client = useBridge();
	const { words } = usePlatform();
	const busy = onboarding.permissions.some((step) => step.isRequesting);
	return (
		<OnboardingPage
			aside={onboarding.retentionSentence}
			footer={
				onboarding.permissionsComplete ? (
					<Button
						data-testid="onboarding-done"
						onClick={() => send(client, "onboarding.advance")}
						variant="primary"
					>
						Continue
					</Button>
				) : (
					<Button
						data-testid="onboarding-later"
						onClick={() => send(client, "onboarding.advance")}
						variant="outline"
					>
						Later
					</Button>
				)
			}
			intro={permissionsIntro(words, onboarding.permissions.length)}
			step={1}
			testId="onboarding-permissions"
			title="Welcome to Steno"
		>
			<FormCard>
				{onboarding.permissions.map((step) => (
					<PermissionRow
						acknowledgeOnly={step.kind === "localNetwork"}
						isRequesting={step.isRequesting}
						key={step.kind}
						kind={step.kind}
						onCheckAgain={() => send(client, "onboarding.refresh")}
						onOpenSystemSettings={() =>
							send(client, "system.openSystemSettings", { kind: step.kind })
						}
						onRequest={() =>
							send(client, "onboarding.request", { kind: step.kind })
						}
						onSkip={
							step.isRequired
								? undefined
								: () => send(client, "onboarding.skip", { kind: step.kind })
						}
						optional={!step.isRequired}
						prominent
						requestDisabled={busy}
						skipped={step.isSkipped}
						state={step.state}
					/>
				))}
			</FormCard>
		</OnboardingPage>
	);
}
