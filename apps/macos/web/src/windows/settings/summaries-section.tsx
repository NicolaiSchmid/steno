import {
	CheckCircle2Icon,
	CircleAlertIcon,
	InfoIcon,
	TriangleAlertIcon,
} from "lucide-react";
import type { SummariesSettingsSnapshot } from "@/bridge/contract";
import { send, useBridge, useSnapshot } from "@/bridge/hooks";
import {
	CODEX_CONSENT,
	CodexConsentCard,
	type CodexState,
} from "@/components/codex-consent-card";
import { DraftField } from "@/components/draft-field";
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
	Select,
} from "@/components/ui";
import { SectionPage } from "./section-page";

/** The consent card until confirmed, then the account line and the model. */
function CodexFields({ codex }: { codex: CodexState }) {
	const client = useBridge();

	if (!codex.confirmed) {
		return (
			<CodexConsentCard
				codex={codex}
				onCheckAgain={() =>
					send(client, "settings.summaries.refreshCodexStatus")
				}
				onConfirm={() => send(client, "settings.summaries.confirmCodex")}
			/>
		);
	}

	return (
		<>
			{codex.signIn === "signedIn" ? (
				<FormRow
					control={<Badge variant="success">Connected</Badge>}
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
							className="w-56"
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
	const codex = summaries.codex;
	// Every field is stored as it is left: one update, then the save.
	const update = (fields: SummariesUpdate & { contextTokens?: string }) => {
		send(client, "settings.summaries.update", fields);
		send(client, "settings.summaries.save");
	};
	const status = statusOf(summaries);

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
				<SummariesEndpointForm
					clearKeyOnCommit
					layout="rows"
					onUpdate={update}
					renderCodex={(state) => <CodexFields codex={state} />}
					summaries={summaries}
				/>
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
								className="w-40"
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
							? "success"
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
