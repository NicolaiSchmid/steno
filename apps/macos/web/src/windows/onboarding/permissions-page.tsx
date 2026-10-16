import {
	CheckCircle2Icon,
	CircleIcon,
	TriangleAlertIcon,
	XCircleIcon,
} from "lucide-react";
import type { OnboardingSnapshot } from "@/bridge/contract";
import { send, useBridge } from "@/bridge/hooks";
import { DownloadFailure, downloadPercent } from "@/components/model-download";
import { PermissionRow } from "@/components/permission-row";
import {
	Badge,
	Button,
	Callout,
	FormCard,
	FormRow,
	type FormRowProps,
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
 * Download, the download's progress, Retry after a failure, or Installed.
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
	const percent = downloadPercent(models.downloadFraction);
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
			control = <Badge variant="success">Installed</Badge>;
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
					? undefined
					: `Steno needs these to turn speech into text and tell the speakers apart. Meetings wait until they are on this ${words.computer}.${status}`
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
				<DownloadFailure
					failure={models.failure}
					testId="onboarding-models-failure"
				/>
			) : null}
		</FormRow>
	);
}

/**
 * The vault row (Tauri shell, Mac only): Steno wrote to the stored vault
 * once, so macOS asked for a protected folder while this page shows. A
 * folder that refused the write says how to allow it, with Open System
 * Settings (Files and Folders) and Check again; a vault that cannot be
 * used otherwise (not there) says to choose it again, with Check again.
 */
function VaultAccessRow({ vault }: { vault: VaultAccess }) {
	const client = useBridge();
	const { words } = usePlatform();
	let description: string;
	let control: React.ReactNode;
	let icon = <CircleIcon aria-hidden="true" />;
	let tone: FormRowProps["tone"] = "faint";
	const checkAgain = (
		<Button
			data-testid="onboarding-vault-check"
			onClick={() => send(client, "onboarding.refresh")}
			size="sm"
			variant="outline"
		>
			Check again
		</Button>
	);
	switch (vault.state) {
		case "checking":
			description = `Steno is checking that it can write your meeting notes to ${vault.name}. If macOS asks about the folder, choose Allow.`;
			control = (
				<Badge data-testid="onboarding-vault-checking">Checking…</Badge>
			);
			break;
		case "writable":
			description = `Steno writes your meeting notes to ${vault.name}.`;
			control = <Badge variant="success">Allowed</Badge>;
			icon = <CheckCircle2Icon aria-hidden="true" />;
			tone = "success";
			break;
		case "refused":
			icon = <XCircleIcon aria-hidden="true" />;
			tone = "warning";
			description = `Steno cannot write your meeting notes to ${vault.name}. Turn on Steno under Files and Folders in ${words.systemSettings}, then choose Check again.`;
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
					{checkAgain}
				</>
			);
			break;
		case "unusable":
			icon = <XCircleIcon aria-hidden="true" />;
			tone = "warning";
			description = `Steno cannot use ${vault.name} for your meeting notes. Check that the folder is there, or choose the vault again in Settings > Export.`;
			control = checkAgain;
			break;
	}
	return (
		<FormRow
			control={control}
			data-testid="onboarding-vault-access"
			description={description}
			icon={icon}
			label="Obsidian vault"
			tone={tone}
		>
			{vault.validationMessage ? (
				<Callout
					data-testid="onboarding-vault-access-message"
					icon={<TriangleAlertIcon aria-hidden="true" />}
					size="sm"
					title={vault.validationMessage}
					variant="warning"
				/>
			) : null}
		</FormRow>
	);
}

/**
 * Page 1: one row per permission with the action that fits its state. The
 * required ones gate Continue; the optional ones can be skipped; the local
 * network prompt comes when Steno first listens for the phone, so its row
 * only explains.
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
