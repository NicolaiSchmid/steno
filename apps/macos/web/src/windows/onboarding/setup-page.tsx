import {
	CheckCircle2Icon,
	CircleAlertIcon,
	CircleIcon,
	FolderIcon,
	TriangleAlertIcon,
} from "lucide-react";
import type { ReactNode } from "react";
import type { BridgeClient } from "@/bridge/client";
import type { OnboardingSnapshot } from "@/bridge/contract";
import { send, useBridge } from "@/bridge/hooks";
import {
	CodexConsentCard,
	codexConsent,
} from "@/components/codex-consent-card";
import {
	SummariesEndpointForm,
	type SummariesUpdate,
} from "@/components/summaries-endpoint-form";
import {
	Badge,
	Button,
	Callout,
	Disclosure,
	FormCard,
	FormRow,
	FormValue,
} from "@/components/ui";
import { type PlatformWords, usePlatform } from "@/lib/platform";
import { OnboardingPage } from "./onboarding-page";

type SetupStep = OnboardingSnapshot["setup"][number];
type SetupKind = SetupStep["kind"];

/** The heading the smoke test finds page 2 by. */
export const SETUP_TITLE = "Summaries and export";

/** One row's words; the Summaries row names the machine as the platform does. */
function setupCopy(
	kind: SetupKind,
	{ computer }: PlatformWords,
): { title: string; explanation: string; footnote: string } {
	switch (kind) {
		case "summaries":
			return {
				title: "Summaries",
				explanation: `Steno sends the transcript text, never audio, to a model to clean it up and write the summary, tasks and decisions: a server or API key of your choice, or your ChatGPT plan through the Codex sign-in on this ${computer}. Without one, meetings keep a raw transcript and no summary.`,
				footnote:
					"The context window and the rest live in Settings > Summaries.",
			};
		case "vault":
			return {
				title: "Obsidian vault",
				explanation:
					"Steno writes each meeting into Meetings/<date>-<slug>/ inside the vault: a folder note, transcript, tasks, VTT and JSON. It never touches files it did not write. Without a vault, meetings stay in Steno.",
				footnote:
					"People pages, the task tag and the audio copy live in Settings > Export.",
			};
	}
}

/**
 * Page 2: the Summaries row (a service and its fields, or the ChatGPT
 * consent card) and the Obsidian vault row, both optional. A saved row
 * collapses to its line; both rows handled finishes the window from the host.
 */
export function SetupPage({ onboarding }: { onboarding: OnboardingSnapshot }) {
	const client = useBridge();
	return (
		<OnboardingPage
			footer={
				<>
					<Button
						data-testid="onboarding-back"
						onClick={() => send(client, "onboarding.back")}
						variant="outline"
					>
						Back
					</Button>
					<Button
						data-testid="onboarding-finish"
						onClick={() => send(client, "onboarding.finish")}
						variant="primary"
					>
						Finish
					</Button>
				</>
			}
			intro="Optional. Steno works as a local transcript recorder without either."
			step={2}
			testId="onboarding-setup"
			title={SETUP_TITLE}
		>
			{onboarding.setup.map((row) => (
				<SetupRow key={row.kind} row={row}>
					{row.kind === "summaries" ? (
						<SummariesFields client={client} onboarding={onboarding} />
					) : (
						<VaultFields client={client} onboarding={onboarding} />
					)}
				</SetupRow>
			))}
		</OnboardingPage>
	);
}

/**
 * One setup row: the state glyph, the title with its Optional badge, the
 * saved line or Skipped badge trailing, and, while open, the explanation
 * with the fields under it and the footnote under the card.
 */
function SetupRow({ row, children }: { row: SetupStep; children: ReactNode }) {
	const copy = setupCopy(row.kind, usePlatform().words);
	const open = row.state === "open";
	return (
		<FormCard footer={open ? copy.footnote : undefined}>
			<FormRow
				control={
					row.state === "saved" ? (
						<FormValue
							data-testid={`setup-${row.kind}-saved`}
							title={row.savedLine}
						>
							{row.savedLine}
						</FormValue>
					) : row.state === "skipped" ? (
						<Badge data-testid={`setup-${row.kind}-skipped`}>Skipped</Badge>
					) : undefined
				}
				data-testid={`setup-${row.kind}`}
				description={open ? copy.explanation : undefined}
				icon={
					row.state === "saved" ? (
						<CheckCircle2Icon aria-hidden="true" />
					) : (
						<CircleIcon aria-hidden="true" />
					)
				}
				label={
					<span className="inline-flex items-center gap-2">
						{copy.title}
						<Badge>Optional</Badge>
					</span>
				}
				tone={row.state === "saved" ? "success" : "faint"}
			>
				{open ? children : null}
			</FormRow>
		</FormCard>
	);
}

function SkipButton({
	client,
	step,
}: {
	client: BridgeClient;
	step: SetupKind;
}) {
	return (
		<Button
			data-testid={`setup-${step}-skip`}
			onClick={() => send(client, "onboarding.skipSetup", { step })}
			size="sm"
			variant="ghost"
		>
			Skip
		</Button>
	);
}

/**
 * The Summaries row's fields: the service, then the server, model and key
 * for an endpoint with Test connection and Save, or the ChatGPT consent
 * card whose button is the save. Text fields send one update when left;
 * nothing is stored until Save.
 */
function SummariesFields({
	client,
	onboarding,
}: {
	client: BridgeClient;
	onboarding: OnboardingSnapshot;
}) {
	const { words } = usePlatform();
	const summaries = onboarding.summaries;
	const skip = <SkipButton client={client} step="summaries" />;
	if (!summaries) {
		return <div className="flex items-center gap-2">{skip}</div>;
	}
	// The draft stays on the page until Save; nothing is stored by an update.
	const update = (fields: SummariesUpdate) =>
		send(client, "settings.summaries.update", fields);

	return (
		<div className="flex flex-col gap-3" data-testid="setup-summaries-form">
			<SummariesEndpointForm
				layout="stack"
				onUpdate={update}
				renderCodex={(codex) =>
					codex.confirmed ? (
						<>
							{codex.signIn === "signedIn" ? (
								<Callout
									data-testid="onboarding-codex-account"
									icon={<CheckCircle2Icon aria-hidden="true" />}
									size="sm"
									title={`Using ChatGPT as ${codex.signInDetail ?? "your account"}.`}
									variant="success"
								/>
							) : codex.signIn === "unavailable" ? (
								<Callout
									data-testid="codex-unavailable"
									icon={<CircleAlertIcon aria-hidden="true" />}
									size="sm"
									title={codex.signInDetail ?? codexConsent(words).noSignIn}
									variant="warning"
								/>
							) : null}
							{codex.modelsError ? (
								<Callout
									data-testid="codex-models-error"
									icon={<TriangleAlertIcon aria-hidden="true" />}
									size="sm"
									title={codex.modelsError}
									variant="warning"
								/>
							) : null}
							<div className="flex items-center gap-2">
								<Button
									data-testid="onboarding-codex-check"
									onClick={() =>
										send(client, "onboarding.confirmSummariesWithCodex")
									}
									size="sm"
									variant="outline"
								>
									Check again
								</Button>
								{skip}
							</div>
						</>
					) : (
						<CodexConsentCard
							codex={codex}
							compact
							onCheckAgain={() =>
								send(client, "settings.summaries.refreshCodexStatus")
							}
							onConfirm={() =>
								send(client, "onboarding.confirmSummariesWithCodex")
							}
							trailing={skip}
						/>
					)
				}
				summaries={summaries}
				testIdPrefix="onboarding-"
			/>
			{summaries.codex ? null : (
				<>
					{summaries.validationMessage ? (
						<Callout
							data-testid="onboarding-summaries-validation"
							icon={<TriangleAlertIcon aria-hidden="true" />}
							size="sm"
							title={summaries.validationMessage}
							variant="warning"
						/>
					) : null}
					{summaries.testResult ? (
						<Callout
							data-testid="onboarding-summaries-test"
							icon={
								summaries.testResult.ok ? (
									<CheckCircle2Icon aria-hidden="true" />
								) : (
									<CircleAlertIcon aria-hidden="true" />
								)
							}
							size="sm"
							title={summaries.testResult.message}
							variant={summaries.testResult.ok ? "success" : "warning"}
						/>
					) : null}
					<div className="flex items-center gap-2">
						<Button
							data-testid="onboarding-test-summaries"
							disabled={
								summaries.isTesting ||
								summaries.baseURL.trim() === "" ||
								summaries.validationMessage !== undefined
							}
							onClick={() => send(client, "settings.summaries.test")}
							size="sm"
							variant="outline"
						>
							{summaries.isTesting ? "Checking…" : "Test connection"}
						</Button>
						<Button
							data-testid="onboarding-save-summaries"
							disabled={!onboarding.canSaveSummaries}
							onClick={() => send(client, "onboarding.saveSummaries")}
							size="sm"
							variant="primary"
						>
							Save
						</Button>
						{skip}
					</div>
				</>
			)}
			{summaries.error ? (
				<Callout
					data-testid="onboarding-summaries-error"
					icon={<CircleAlertIcon aria-hidden="true" />}
					size="sm"
					title={summaries.error}
					variant="warning"
				/>
			) : null}
		</div>
	);
}

/**
 * The vault row's controls: the chosen folder, the native chooser (which
 * saves the choice at once), Try again after a refused folder, and Skip.
 */
function VaultFields({
	client,
	onboarding,
}: {
	client: BridgeClient;
	onboarding: OnboardingSnapshot;
}) {
	const vault = onboarding.vault;
	const skip = <SkipButton client={client} step="vault" />;
	if (!vault) {
		return <div className="flex items-center gap-2">{skip}</div>;
	}
	return (
		<div className="flex flex-col gap-3" data-testid="setup-vault-form">
			{vault.path ? (
				<FormValue
					data-testid="onboarding-vault-name"
					icon={<FolderIcon aria-hidden="true" />}
					title={vault.path}
				>
					{vault.name ?? vault.path}
				</FormValue>
			) : null}
			{vault.validationMessage ? (
				<Callout
					data-testid="onboarding-vault-validation"
					icon={<TriangleAlertIcon aria-hidden="true" />}
					size="sm"
					title={vault.validationMessage}
					variant="warning"
				/>
			) : null}
			{vault.error ? (
				<div className="flex flex-col gap-1.5">
					<Callout
						data-testid="onboarding-vault-error"
						icon={<CircleAlertIcon aria-hidden="true" />}
						size="sm"
						title={vault.error}
						variant="warning"
					/>
					{vault.errorDetails ? (
						<Disclosure>{vault.errorDetails}</Disclosure>
					) : null}
				</div>
			) : null}
			<div className="flex items-center gap-2">
				<Button
					data-testid="onboarding-choose-vault"
					onClick={() => send(client, "onboarding.chooseVault")}
					size="sm"
					variant={vault.path ? "outline" : "primary"}
				>
					{vault.path ? "Choose another…" : "Choose vault…"}
				</Button>
				{vault.path ? (
					<Button
						data-testid="onboarding-save-vault"
						onClick={() => send(client, "onboarding.saveVault")}
						size="sm"
						variant="outline"
					>
						Try again
					</Button>
				) : null}
				{skip}
			</div>
		</div>
	);
}
