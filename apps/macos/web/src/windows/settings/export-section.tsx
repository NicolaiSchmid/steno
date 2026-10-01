import {
	CheckCircle2Icon,
	FolderIcon,
	InfoIcon,
	TriangleAlertIcon,
} from "lucide-react";
import type { ExportSettingsSnapshot } from "@/bridge/contract";
import { send, useBridge, useSnapshot } from "@/bridge/hooks";
import { DraftField } from "@/components/draft-field";
import {
	Button,
	Callout,
	FormCard,
	FormRow,
	FormValue,
	Switch,
} from "@/components/ui";
import { SectionPage } from "./section-page";

/**
 * What exporting amounts to right now; null while the page's own error row
 * already says why nothing was saved.
 */
export function exportStatus(
	snapshot: ExportSettingsSnapshot,
): { kind: "info" | "warning" | "success"; text: string } | null {
	if (!snapshot.enabled) {
		return null;
	}
	if (!snapshot.vaultPath) {
		return { kind: "info", text: "Choose a vault folder to start exporting." };
	}
	if (snapshot.validationMessage) {
		return { kind: "warning", text: snapshot.validationMessage };
	}
	if (snapshot.error) {
		return null;
	}
	return {
		kind: "success",
		text: `Exporting to ${snapshot.vaultName ?? "the vault"}. New meetings are written there when they finish.`,
	};
}

/**
 * Export: the Obsidian switch, the vault, the people folder, the task tag
 * and whether the recording is copied along; then what exporting amounts to.
 */
export function ExportSection() {
	const client = useBridge();
	const exportSettings = useSnapshot("settings.export");
	if (!exportSettings) {
		return <SectionPage id="export" />;
	}
	const update = (fields: {
		peopleFolder?: string;
		includeAudio?: boolean;
		taskTag?: string;
	}) => {
		send(client, "settings.export.update", fields);
		send(client, "settings.export.save");
	};
	const status = exportStatus(exportSettings);

	return (
		<SectionPage
			error={exportSettings.error}
			errorDetails={exportSettings.errorDetails}
			id="export"
		>
			<FormCard
				footer={
					exportSettings.enabled
						? "Each meeting becomes a folder with a note, the transcript and the tasks. Steno never edits files it did not create."
						: undefined
				}
			>
				<FormRow
					control={
						<Switch
							aria-label="Export to Obsidian"
							checked={exportSettings.enabled}
							data-testid="export-enabled"
							onCheckedChange={(value) =>
								send(client, "settings.export.setEnabled", { value })
							}
						/>
					}
					label="Export to Obsidian"
				/>
				{exportSettings.enabled ? (
					<FormRow
						control={
							<>
								{exportSettings.vaultName ? (
									<FormValue
										data-testid="vault-name"
										icon={<FolderIcon aria-hidden="true" />}
										title={exportSettings.vaultPath}
									>
										{exportSettings.vaultName}
									</FormValue>
								) : null}
								<Button
									data-testid="choose-vault"
									onClick={() => send(client, "settings.export.chooseVault")}
									size="sm"
									variant="outline"
								>
									Choose…
								</Button>
							</>
						}
						label="Vault"
					/>
				) : null}
			</FormCard>

			{exportSettings.enabled ? (
				<FormCard
					footer="One page per person, inside the vault. Leave empty for none."
					title="Advanced"
				>
					<FormRow
						control={
							<DraftField
								className="w-40"
								label="People folder"
								onCommit={(peopleFolder) => update({ peopleFolder })}
								placeholder="People"
								testId="people-folder"
								value={exportSettings.peopleFolder}
							/>
						}
						label="People folder"
					/>
					<FormRow
						control={
							<DraftField
								className="w-40"
								label="Tag for tasks"
								onCommit={(taskTag) => update({ taskTag })}
								placeholder="task"
								testId="task-tag"
								value={exportSettings.taskTag}
							/>
						}
						label="Tag for tasks"
					/>
					<FormRow
						control={
							<Switch
								aria-label="Copy the recording into the vault"
								checked={exportSettings.includeAudio}
								data-testid="include-audio"
								onCheckedChange={(includeAudio) =>
									send(client, "settings.export.update", { includeAudio })
								}
							/>
						}
						label="Copy the recording into the vault"
					/>
				</FormCard>
			) : null}

			{status ? (
				<Callout
					data-testid="export-status"
					icon={
						status.kind === "success" ? (
							<CheckCircle2Icon aria-hidden="true" />
						) : status.kind === "warning" ? (
							<TriangleAlertIcon aria-hidden="true" />
						) : (
							<InfoIcon aria-hidden="true" />
						)
					}
					title={status.text}
					variant={status.kind}
				/>
			) : null}
		</SectionPage>
	);
}
