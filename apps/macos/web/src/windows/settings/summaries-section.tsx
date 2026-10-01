import {
	CheckCircle2Icon,
	CircleAlertIcon,
	InfoIcon,
	TriangleAlertIcon,
} from "lucide-react";
import type { SummariesSettingsSnapshot } from "@/bridge/contract";
import { send, useBridge, useSnapshot } from "@/bridge/hooks";
import {
	Badge,
	Button,
	Callout,
	Card,
	Disclosure,
	FormCard,
	FormRow,
	Input,
	Select,
} from "@/components/ui";
import { DraftField } from "./draft-field";
import { SectionPage } from "./section-page";

type Codex = NonNullable<SummariesSettingsSnapshot["codex"]>;

/** The words the user reads before Steno may use the Codex sign-in. */
const CODEX_CONSENT = {
	title: "Use your ChatGPT plan for summaries",
	body: "Steno will use the sign-in that the Codex command-line tool saved on this Mac (~/.codex/auth.json) and send your meeting transcripts to OpenAI under your ChatGPT plan. Audio never leaves your Mac.",
	points: [
		"Summaries count against your ChatGPT plan's Codex limits, shared with your coding sessions.",
		"Steno refreshes the saved sign-in when it expires and writes the new one back to the same file, the same way Codex does.",
		"OpenAI allows tools like this today but has not promised to keep doing so. If it stops working, switch to an API key or a local model in Settings.",
	],
	confirm: "Use my ChatGPT account",
	checkAgain: "Check again",
	usageFootnote:
		"Transcript text goes to OpenAI under your ChatGPT plan and counts against its Codex limits. Audio never leaves your Mac.",
};

/** The consent card until confirmed, then the account line and the model. */
function CodexFields({ codex }: { codex: Codex }) {
	const client = useBridge();
	const signedIn = codex.signIn === "signedIn";

	if (!codex.confirmed) {
		return (
			<Card
				className="flex flex-col gap-3"
				data-testid="codex-consent"
				padding="lg"
			>
				<div className="flex flex-col gap-1.5">
					<h3 className="m-0 font-semibold text-[13px]">
						{CODEX_CONSENT.title}
					</h3>
					<p className="m-0 text-[13px] text-muted-foreground leading-[1.45]">
						{CODEX_CONSENT.body}
					</p>
				</div>
				<ul className="m-0 flex list-disc flex-col gap-1 pl-4 text-[12px] text-muted-foreground leading-[1.45]">
					{CODEX_CONSENT.points.map((point) => (
						<li key={point}>{point}</li>
					))}
				</ul>
				{codex.signIn === "signedIn" ? (
					<Callout
						data-testid="codex-signed-in"
						icon={<InfoIcon aria-hidden="true" />}
						size="sm"
						title={`Signed in as ${codex.signInDetail ?? "your ChatGPT account"}.`}
						variant="info"
					/>
				) : codex.signIn === "unavailable" ? (
					<Callout
						data-testid="codex-unavailable"
						icon={<TriangleAlertIcon aria-hidden="true" />}
						size="sm"
						title={
							codex.signInDetail ?? "No ChatGPT sign-in was found on this Mac."
						}
						variant="warning"
					/>
				) : null}
				<div className="flex items-center gap-2">
					<Button
						data-testid="codex-confirm"
						disabled={!signedIn}
						onClick={() => send(client, "settings.summaries.confirmCodex")}
						size="sm"
						variant="primary"
					>
						{CODEX_CONSENT.confirm}
					</Button>
					{signedIn ? null : (
						<Button
							data-testid="codex-check-again"
							onClick={() =>
								send(client, "settings.summaries.refreshCodexStatus")
							}
							size="sm"
							variant="outline"
						>
							{CODEX_CONSENT.checkAgain}
						</Button>
					)}
				</div>
			</Card>
		);
	}

	return (
		<>
			{codex.signIn === "signedIn" ? (
				<FormRow
					control={<Badge variant="live">Connected</Badge>}
					data-testid="codex-account"
					label={`Using ChatGPT as ${codex.signInDetail ?? "your account"}.`}
				/>
			) : codex.signIn === "unavailable" ? (
				<FormRow label="ChatGPT sign-in">
					<Callout
						data-testid="codex-unavailable"
						icon={<CircleAlertIcon aria-hidden="true" />}
						size="sm"
						title={
							codex.signInDetail ?? "No ChatGPT sign-in was found on this Mac."
						}
						variant="warning"
					/>
				</FormRow>
			) : null}
			<FormRow
				control={
					<>
						<Select
							aria-label="Model"
							className="w-[220px]"
							data-testid="codex-model"
							disabled={codex.models.length === 0}
							onValueChange={(value) => {
								if (value) {
									send(client, "settings.summaries.selectCodexModel", {
										value,
									});
								}
							}}
							options={codex.models.map((model) => ({
								value: model.slug,
								label: model.name,
							}))}
							placeholder="pick a model"
							size="sm"
							value={codex.model || null}
						/>
						<Button
							data-testid="codex-refresh"
							disabled={codex.isLoadingModels}
							onClick={() =>
								send(client, "settings.summaries.refreshCodexModels")
							}
							size="sm"
							variant="ghost"
						>
							{codex.isLoadingModels ? "Loading…" : "Refresh"}
						</Button>
					</>
				}
				label="Model"
			>
				{codex.modelsError ? (
					<Callout
						data-testid="codex-models-error"
						icon={<TriangleAlertIcon aria-hidden="true" />}
						size="sm"
						title={codex.modelsError}
						variant="warning"
					/>
				) : null}
			</FormRow>
			<FormRow
				control={
					<Button
						data-testid="codex-stop"
						onClick={() => send(client, "settings.summaries.stopUsingCodex")}
						size="sm"
						variant="ghost"
					>
						Stop using ChatGPT
					</Button>
				}
				label="Switch to a server or API key instead"
			/>
		</>
	);
}

/** What the status row says, from the test state and the configuration. */
function statusOf(summaries: SummariesSettingsSnapshot): {
	title: string;
	detail?: string;
	kind: "info" | "ok" | "failed";
} {
	if (summaries.isTesting) {
		return { title: "Checking the connection…", kind: "info" };
	}
	if (summaries.testResult) {
		return summaries.testResult.ok
			? {
					title: "Connected.",
					detail: summaries.testResult.message,
					kind: "ok",
				}
			: {
					title: "Could not connect to the service.",
					detail: summaries.testResult.message,
					kind: "failed",
				};
	}
	return summaries.isConfigured
		? { title: "Saved. The connection has not been checked yet.", kind: "info" }
		: {
				title:
					"Not set up. Choose a service and enter a model name; summaries stay off until then.",
				kind: "info",
			};
}

/**
 * Summaries: the service preset, the server, model and key for an endpoint,
 * or the ChatGPT consent and model; then the connection status.
 */
export function SummariesSection() {
	const client = useBridge();
	const summaries = useSnapshot("settings.summaries");
	if (!summaries) {
		return <SectionPage id="summaries" />;
	}
	const preset = summaries.presets.find(
		(candidate) => candidate.id === summaries.presetID,
	);
	const codex = summaries.codex;
	const update = (fields: {
		baseURL?: string;
		model?: string;
		contextTokens?: string;
		apiKey?: string;
	}) => {
		send(client, "settings.summaries.update", fields);
		send(client, "settings.summaries.save");
	};
	const status = statusOf(summaries);
	const keyPlaceholder = summaries.hasAPIKey
		? "Saved in your keychain"
		: preset?.needsAPIKey
			? `Paste the key from your ${preset.title} account`
			: "Only if the server needs one";

	return (
		<SectionPage
			error={summaries.error}
			errorDetails={summaries.errorDetails}
			id="summaries"
		>
			<FormCard
				footer={
					codex
						? codex.confirmed
							? CODEX_CONSENT.usageFootnote
							: undefined
						: "Stored in your login keychain and sent only to the server above."
				}
			>
				<FormRow
					control={
						<Select
							aria-label="Service"
							className="w-[220px]"
							data-testid="preset"
							onValueChange={(value) => {
								if (value) {
									send(client, "settings.summaries.selectPreset", { value });
								}
							}}
							options={summaries.presets.map((candidate) => ({
								value: candidate.id,
								label: candidate.title,
							}))}
							size="sm"
							value={summaries.presetID}
						/>
					}
					label="Service"
				/>
				{codex ? (
					<CodexFields codex={codex} />
				) : (
					<>
						{preset?.showsServerField ? (
							<FormRow
								control={
									<DraftField
										label="Server address"
										onCommit={(baseURL) => update({ baseURL })}
										placeholder="Starts with http or https"
										testId="base-url"
										value={summaries.baseURL}
									/>
								}
								label="Server address"
							/>
						) : null}
						<FormRow
							control={
								<DraftField
									label="Model"
									onCommit={(model) => update({ model })}
									placeholder={preset?.modelPlaceholder}
									testId="model"
									value={summaries.model}
								/>
							}
							label="Model"
						/>
						<FormRow
							control={
								<DraftField
									clearOnCommit
									label="API key"
									onCommit={(apiKey) => {
										if (apiKey.trim()) {
											update({ apiKey });
										}
									}}
									placeholder={keyPlaceholder}
									testId="api-key"
									type="password"
									value=""
								/>
							}
							description={
								summaries.hasAPIKey
									? "A key is stored. Paste a new one to replace it."
									: undefined
							}
							label="API key"
						/>
					</>
				)}
			</FormCard>
			{summaries.validationMessage ? (
				<Callout
					data-testid="validation"
					icon={<TriangleAlertIcon aria-hidden="true" />}
					title={summaries.validationMessage}
					variant="warning"
				/>
			) : null}

			{codex ? null : (
				<FormCard
					footer={`How much text the model can read at once. Leave the default of ${summaries.defaultContextTokens.toLocaleString()} unless the service reports a shorter limit.`}
					title="Advanced"
				>
					<FormRow
						control={
							<DraftField
								label="Context size"
								onCommit={(contextTokens) => update({ contextTokens })}
								testId="context-tokens"
								value={summaries.contextTokens}
							/>
						}
						label="Context size"
					/>
				</FormCard>
			)}

			<FormCard title="Connection">
				<FormRow
					control={
						summaries.isConfigured ? (
							<Button
								data-testid="test-connection"
								disabled={summaries.isTesting}
								onClick={() => send(client, "settings.summaries.test")}
								size="sm"
								variant="outline"
							>
								Test again
							</Button>
						) : undefined
					}
					data-testid="summaries-status"
					icon={
						status.kind === "ok" ? (
							<CheckCircle2Icon aria-hidden="true" />
						) : status.kind === "failed" ? (
							<CircleAlertIcon aria-hidden="true" />
						) : (
							<InfoIcon aria-hidden="true" />
						)
					}
					label={status.title}
					tone={
						status.kind === "ok"
							? "primary"
							: status.kind === "failed"
								? "warning"
								: "faint"
					}
				>
					{status.detail ? (
						<Disclosure
							data-testid="test-result-details"
							defaultOpen={status.kind === "failed"}
						>
							{status.detail}
						</Disclosure>
					) : null}
				</FormRow>
			</FormCard>
		</SectionPage>
	);
}
