import type { OnboardingSnapshot } from "@/bridge/contract";
import { send, useBridge } from "@/bridge/hooks";
import { PermissionRow } from "@/components/permission-row";
import { Button, FormCard } from "@/components/ui";
import { OnboardingPage } from "./onboarding-page";

/** The sentence the smoke test finds page 1 by. */
export const PERMISSIONS_INTRO =
	"A few permissions, then where summaries come from and where meetings go. Audio never leaves this Mac.";

/**
 * Page 1: one row per permission with the action that fits its state. The
 * required ones gate Done; the optional ones can be skipped; the local
 * network prompt comes with the first pairing, so its row only explains.
 * Later moves on with whatever is still open.
 */
export function PermissionsPage({
	onboarding,
}: {
	onboarding: OnboardingSnapshot;
}) {
	const client = useBridge();
	const busy = onboarding.permissions.some((step) => step.isRequesting);
	return (
		<OnboardingPage
			aside={onboarding.retentionSentence}
			footer={
				onboarding.permissionsComplete ? (
					<Button
						data-testid="onboarding-done"
						onClick={() => send(client, "onboarding.advance")}
						size="md"
						variant="primary"
					>
						Done
					</Button>
				) : (
					<Button
						data-testid="onboarding-later"
						onClick={() => send(client, "onboarding.advance")}
						size="md"
						variant="outline"
					>
						Later
					</Button>
				)
			}
			intro={PERMISSIONS_INTRO}
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
