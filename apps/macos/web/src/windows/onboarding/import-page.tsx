import { KeyRoundIcon, TriangleAlertIcon } from "lucide-react";
import type { OnboardingSnapshot } from "@/bridge/contract";
import { send, useBridge } from "@/bridge/hooks";
import { Button, Callout } from "@/components/ui";
import { OnboardingPage } from "./onboarding-page";

/**
 * What the step brings over and the prompts that come with it, at most
 * three: one per keychain item the host counted (the API key, whichever
 * version stored it, the phone pairing, and the pairing a beta left
 * behind, which the import replaces).
 */
function importIntro(prompts: number): string {
	const what =
		"Steno brings over this Mac's phone pairing from the previous version, so your phone keeps uploading without pairing again, and your API key for summaries if the previous version kept one.";
	if (prompts < 1) {
		return what;
	}
	const times =
		prompts === 1
			? "once"
			: prompts === 2
				? "up to two times"
				: prompts === 3
					? "up to three times"
					: `up to ${prompts} times`;
	return `${what} macOS asks for your login password ${times}.`;
}

/**
 * The import step before page 1, on the Mac's first launch after the update
 * from the previous Steno: it says which keychain prompts come and that
 * Always Allow is the answer, then Continue runs the import. A denied or
 * failed export leaves the step open with the reason and Try again; phone
 * uploads wait until then. Not now (Continue for now once it failed) goes on
 * to page 1, or to page 2 when the permissions are already granted, and the
 * step returns at the next launch.
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
						{waiting ? "Continue for now" : "Not now"}
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
				description={`Then Steno reads what the previous version stored without asking again. Your meetings and settings are already here. ${
					waiting
						? "Continue for now leaves phone uploads waiting until this step comes back at the next launch."
						: "Not now leaves phone uploads waiting, and summaries without the API key, until this step comes back at the next launch."
				}`}
				icon={<KeyRoundIcon aria-hidden="true" />}
				title="Choose Always Allow in each prompt."
				variant="info"
			/>
			{waiting ? (
				<Callout
					data-testid="import-waiting"
					description="Until it comes over, your phone cannot upload to this Mac."
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
