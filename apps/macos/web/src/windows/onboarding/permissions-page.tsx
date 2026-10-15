import {
	CheckCircle2Icon,
	CircleAlertIcon,
	CircleIcon,
	XCircleIcon,
} from "lucide-react";
import type { OnboardingSnapshot } from "@/bridge/contract";
import { send, useBridge } from "@/bridge/hooks";
import { PermissionRow } from "@/components/permission-row";
import {
	Badge,
	Button,
	Callout,
	Disclosure,
	FormCard,
	FormRow,
	ProgressBar,
} from "@/components/ui";
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

type SpeechModels = NonNullable<OnboardingSnapshot["speechModels"]>;
type VaultAccess = NonNullable<OnboardingSnapshot["vaultAccess"]>;

/**
 * The speech models row (Tauri shell only): what a meeting waits for, with
 * Download, the download's progress, Retry after a failure, or Downloaded.
 * Download is Settings' install, once per model the row lists.
 */
function SpeechModelsRow({ models }: { models: SpeechModels }) {
	const client = useBridge();
	const { words } = usePlatform();
	const download = () => {
		for (const assetID of models.assetIDs) {
			send(client, "settings.transcription.download", { assetID });
		}
	};
	const percent =
		models.downloadFraction !== undefined && models.downloadFraction > 0
			? Math.round(models.downloadFraction * 100)
			: null;
	let control: React.ReactNode;
	switch (models.state) {
		case "absent":
			control = (
				<Button
					data-testid="onboarding-models-download"
					onClick={download}
					size="sm"
					variant="primary"
				>
					Download
				</Button>
			);
			break;
		case "failed":
			control = (
				<Button
					data-testid="onboarding-models-retry"
					onClick={download}
					size="sm"
					variant="outline"
				>
					Retry
				</Button>
			);
			break;
		case "downloading":
			control = (
				<span
					className="font-mono text-faint text-xs tabular-nums"
					data-testid="onboarding-models-progress"
				>
					{percent === null ? "…" : `${percent}%`}
				</span>
			);
			break;
		case "installed":
			control = <Badge variant="success">Downloaded</Badge>;
			break;
	}
	const installed = models.state === "installed";
	// The bar and the percentage show a download's progress; the size goes
	// in the sentence otherwise.
	const status = models.state === "downloading" ? "" : ` ${models.detail}.`;
	return (
		<FormRow
			control={control}
			data-testid="onboarding-models"
			description={
				installed
					? models.detail
					: `Steno turns speech into text and tells the speakers apart on this ${words.computer}. Meetings wait until these are downloaded.${status}`
			}
			icon={
				installed ? (
					<CheckCircle2Icon aria-hidden="true" />
				) : (
					<CircleIcon aria-hidden="true" />
				)
			}
			label="Speech models"
			tone={installed ? "success" : "faint"}
		>
			{models.state === "downloading" ? (
				<ProgressBar
					aria-label="Downloading the speech models"
					value={percent}
				/>
			) : null}
			{models.state === "failed" ? (
				<div className="flex flex-col gap-1.5">
					<Callout
						icon={<CircleAlertIcon aria-hidden="true" />}
						size="sm"
						title="The download did not finish."
						variant="warning"
					/>
					{models.failure ? (
						<Disclosure data-testid="onboarding-models-failure">
							{models.failure}
						</Disclosure>
					) : null}
				</div>
			) : null}
		</FormRow>
	);
}

/**
 * The vault row (Tauri shell, Mac only): Steno wrote to the stored vault
 * once, so macOS asked for a protected folder while this page shows. A
 * refusal says how to allow it, with Open System Settings (Files and
 * Folders) and Check again.
 */
function VaultAccessRow({ vault }: { vault: VaultAccess }) {
	const client = useBridge();
	const { words } = usePlatform();
	let description: string;
	let control: React.ReactNode;
	switch (vault.state) {
		case "checking":
			description = `Steno is checking that it can write your notes to ${vault.name}. If macOS asks for the folder, choose Allow.`;
			control = (
				<Badge data-testid="onboarding-vault-checking">Checking…</Badge>
			);
			break;
		case "writable":
			description = `Steno writes your meeting notes to ${vault.name}.`;
			control = <Badge variant="success">Allowed</Badge>;
			break;
		case "refused":
			description = `Steno cannot write your notes to ${vault.name}. Turn on Steno under Files and Folders in ${words.systemSettings}, then choose Check again.`;
			control = (
				<>
					<Button
						data-testid="onboarding-vault-open"
						onClick={() => send(client, "onboarding.openFilesAndFolders")}
						size="sm"
						variant="outline"
					>
						Open {words.systemSettings}
					</Button>
					<Button
						data-testid="onboarding-vault-check"
						onClick={() => send(client, "onboarding.refresh")}
						size="sm"
						variant="outline"
					>
						Check again
					</Button>
				</>
			);
			break;
	}
	return (
		<FormRow
			control={control}
			data-testid="onboarding-vault-access"
			description={description}
			icon={
				vault.state === "writable" ? (
					<CheckCircle2Icon aria-hidden="true" />
				) : vault.state === "refused" ? (
					<XCircleIcon aria-hidden="true" />
				) : (
					<CircleIcon aria-hidden="true" />
				)
			}
			label="Notes folder"
			tone={
				vault.state === "writable"
					? "success"
					: vault.state === "refused"
						? "warning"
						: "faint"
			}
		>
			{vault.state === "refused" && vault.error ? (
				<Disclosure data-testid="onboarding-vault-access-error">
					{vault.error}
				</Disclosure>
			) : null}
		</FormRow>
	);
}

/**
 * Page 1: one row per permission with the action that fits its state. The
 * required ones gate Continue; the optional ones can be skipped; the local
 * network prompt comes with the first pairing, so its row only explains.
 * Later moves on with whatever is still open. Below them, when the host
 * sends them, the speech models row and the vault row.
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
						waitsForRecording={step.waitsForRecording}
					/>
				))}
			</FormCard>
			{onboarding.speechModels || onboarding.vaultAccess ? (
				<FormCard>
					{onboarding.speechModels ? (
						<SpeechModelsRow models={onboarding.speechModels} />
					) : null}
					{onboarding.vaultAccess ? (
						<VaultAccessRow vault={onboarding.vaultAccess} />
					) : null}
				</FormCard>
			) : null}
		</OnboardingPage>
	);
}
