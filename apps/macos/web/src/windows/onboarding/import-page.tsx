import { KeyRoundIcon, TriangleAlertIcon } from "lucide-react";
import type { OnboardingSnapshot } from "@/bridge/contract";
import { send, useBridge } from "@/bridge/hooks";
import { Button, Callout } from "@/components/ui";
import { OnboardingPage } from "./onboarding-page";

/**
 * What the step brings over and the prompts that come with it: two with
 * the API key, one with the phone pairing alone.
 */
function importIntro(prompts: number): string {
	return prompts > 1
		? "Steno brings over your API key for summaries and this Mac's phone pairing from the previous version. macOS asks for your login password twice, once for each."
		: "Steno brings over this Mac's phone pairing from the previous version, so your phone keeps uploading without pairing again. macOS asks for your login password once.";
}

/**
 * The import step before page 1, on the Mac's first launch after the update
 * from the previous Steno: it says which keychain prompts come and that
 * Always Allow is the answer, then Continue runs the import. A denied or
 * failed export leaves the step open with the reason and Try again; phone
 * uploads wait until then. Not now (and Continue without phones once it
 * failed) goes on to page 1, and the step returns at the next launch.
 */
export function ImportPage({ onboarding }: { onboarding: OnboardingSnapshot }) {
	const client = useBridge();
	const step = onboarding.swiftImport;
	if (!step) {
		return null;
	}
	const importing = step.state === "importing";
	const waiting = step.state === "waiting";
	const run = () => send(client, "onboarding.import");
	const skip = () => send(client, "onboarding.skipImport");
	return (
		<OnboardingPage
			footer={
				<>
					<Button
						data-testid="import-skip"
						disabled={importing}
						onClick={skip}
						variant="outline"
					>
						{waiting ? "Continue without phones" : "Not now"}
					</Button>
					<Button
						data-testid="import-run"
						disabled={importing}
						onClick={run}
						variant="primary"
					>
						{importing
							? "Waiting for macOS…"
							: waiting
								? "Try again"
								: "Continue"}
					</Button>
				</>
			}
			intro={importIntro(step.prompts)}
			testId="onboarding-import"
			title="Welcome to the new Steno"
		>
			<Callout
				data-testid="import-always-allow"
				description="Then Steno reads what the previous version stored without asking again. Your meetings and settings are already here."
				icon={<KeyRoundIcon aria-hidden="true" />}
				title="Choose Always Allow in each prompt."
				variant="info"
			/>
			{waiting ? (
				<Callout
					data-testid="import-waiting"
					description="Until then, your phone cannot upload to this Mac."
					icon={<TriangleAlertIcon aria-hidden="true" />}
					title={
						step.error ?? "This Mac's phone pairing has not been brought over."
					}
					variant="warning"
				/>
			) : null}
		</OnboardingPage>
	);
}
