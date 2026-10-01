import type { ReactNode } from "react";
import type { SummariesSettingsSnapshot } from "@/bridge/contract";
import { send, useBridge } from "@/bridge/hooks";
import type { CodexState } from "@/components/codex-consent-card";
import { DraftField } from "@/components/draft-field";
import { FormRow, Select } from "@/components/ui";

export interface SummariesUpdate {
	baseURL?: string;
	model?: string;
	apiKey?: string;
}

export interface SummariesEndpointFormProps {
	summaries: SummariesSettingsSnapshot;
	/**
	 * `rows`: Settings' labelled `FormRow`s, for a `FormCard`; `stack`:
	 * onboarding's full-width fields with the field's name in the placeholder,
	 * for a column.
	 */
	layout: "rows" | "stack";
	/** `""` in Settings, `"onboarding-"` in onboarding: `<prefix>preset`, `<prefix>base-url`, `<prefix>model`, `<prefix>api-key`. */
	testIdPrefix?: string;
	/** A text field was left with a new value. */
	onUpdate: (fields: SummariesUpdate) => void;
	/**
	 * Empty the key field once sent: Settings stores it at once and must not
	 * keep it on the page; onboarding keeps the draft until Save.
	 */
	clearKeyOnCommit?: boolean;
	/** What replaces the fields while ChatGPT is the service: the two pages' own commands. */
	renderCodex: (codex: CodexState) => ReactNode;
}

/**
 * The Summaries endpoint form both windows draw: the service, then the
 * server address, model and API key for an endpoint, or the page's ChatGPT
 * block. Choosing a service sends `settings.summaries.selectPreset` (the
 * host commits the preset's address and model at once); the fields keep a
 * draft and report through `onUpdate` when left. The two hosts answer the
 * same method names on their own model.
 */
export function SummariesEndpointForm({
	summaries,
	layout,
	testIdPrefix = "",
	onUpdate,
	clearKeyOnCommit = false,
	renderCodex,
}: SummariesEndpointFormProps) {
	const client = useBridge();
	const stacked = layout === "stack";
	const preset = summaries.presets.find(
		(candidate) => candidate.id === summaries.presetID,
	);
	const keyPlaceholder = summaries.hasAPIKey
		? "Saved in your keychain"
		: preset?.needsAPIKey
			? `Paste the key from your ${preset.title} account`
			: stacked
				? "API key, only if the server needs one"
				: "Only if the server needs one";
	const fieldClass = stacked ? "w-full" : "w-[200px]";

	const service = (
		<Select
			aria-label="Service"
			className={stacked ? "w-[260px]" : "w-[220px]"}
			data-testid={`${testIdPrefix}preset`}
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
	);
	const server = preset?.showsServerField ? (
		<DraftField
			className={fieldClass}
			label="Server address"
			onCommit={(baseURL) => onUpdate({ baseURL })}
			placeholder={
				stacked
					? "Server address, starting with http or https"
					: "Starts with http or https"
			}
			testId={`${testIdPrefix}base-url`}
			value={summaries.baseURL}
		/>
	) : null;
	const model = (
		<DraftField
			className={fieldClass}
			label="Model"
			onCommit={(value) => onUpdate({ model: value })}
			placeholder={
				stacked
					? `Model: ${preset?.modelPlaceholder ?? "the model name"}`
					: preset?.modelPlaceholder
			}
			testId={`${testIdPrefix}model`}
			value={summaries.model}
		/>
	);
	const apiKey = (
		<DraftField
			className={fieldClass}
			clearOnCommit={clearKeyOnCommit}
			label="API key"
			onCommit={(value) => {
				if (value.trim()) {
					onUpdate({ apiKey: value });
				}
			}}
			placeholder={keyPlaceholder}
			testId={`${testIdPrefix}api-key`}
			type="password"
			value=""
		/>
	);

	if (stacked) {
		return (
			<>
				{service}
				{summaries.codex ? (
					renderCodex(summaries.codex)
				) : (
					<>
						{server}
						{model}
						{apiKey}
					</>
				)}
			</>
		);
	}
	return (
		<>
			<FormRow control={service} label="Service" />
			{summaries.codex ? (
				renderCodex(summaries.codex)
			) : (
				<>
					{server ? <FormRow control={server} label="Server address" /> : null}
					<FormRow control={model} label="Model" />
					<FormRow
						control={apiKey}
						description={
							summaries.hasAPIKey
								? "A key is stored. Paste a new one to replace it."
								: undefined
						}
						label="API key"
					/>
				</>
			)}
		</>
	);
}
