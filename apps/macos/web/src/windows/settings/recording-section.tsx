import { FolderIcon, RefreshCwIcon } from "lucide-react";
import type { RecordingSettingsSnapshot } from "@/bridge/contract";
import { send, useBridge, useSnapshot } from "@/bridge/hooks";
import { PermissionRow } from "@/components/permission-row";
import {
	Button,
	FormCard,
	FormRow,
	FormValue,
	Input,
	Select,
} from "@/components/ui";
import { useDraft } from "@/lib/use-draft";
import { SectionPage } from "./section-page";
import { folderUsageText, retentionTitle } from "./settings-format";

type RetentionMode = RecordingSettingsSnapshot["retention"]["mode"];

/** The picker's options, in display order: the safe choice first. */
const RETENTION_MODES: readonly RetentionMode[] = [
	"keepForever",
	"keepDays",
	"deleteAfterProcessing",
];
const DAY_RANGE = { min: 1, max: 3650 };
/** The picker's value for the system default input (an empty uid on the wire). */
const DEFAULT_INPUT = "system-default";

/** The "Keep recordings" row: the rule, and the number of days when it counts. */
function RetentionRow({
	retention,
	keptForeverCount,
}: {
	retention: RecordingSettingsSnapshot["retention"];
	keptForeverCount: number | undefined;
}) {
	const client = useBridge();
	const [days, setDays] = useDraft(String(retention.days));

	const commitDays = () => {
		const parsed = Number.parseInt(days, 10);
		if (Number.isNaN(parsed)) {
			setDays(String(retention.days));
			return;
		}
		const clamped = Math.min(DAY_RANGE.max, Math.max(DAY_RANGE.min, parsed));
		if (clamped !== retention.days) {
			send(client, "settings.recording.setRetention", {
				retention: { mode: "keepDays", days: clamped },
			});
		} else {
			setDays(String(clamped));
		}
	};

	return (
		<FormRow
			control={
				<>
					{retention.mode === "keepDays" ? (
						<Input
							aria-label="Days"
							className="w-16"
							data-testid="retention-days"
							inputMode="numeric"
							max={DAY_RANGE.max}
							min={DAY_RANGE.min}
							onBlur={commitDays}
							onKeyDown={(event) => {
								if (event.key === "Enter") {
									event.currentTarget.blur();
								}
							}}
							onValueChange={(value) => setDays(String(value))}
							size="sm"
							type="number"
							value={days}
						/>
					) : null}
					<Select
						aria-label="Keep recordings"
						className="w-56"
						data-testid="retention-mode"
						onValueChange={(mode) => {
							if (mode) {
								send(client, "settings.recording.setRetention", {
									retention: { mode, days: retention.days },
								});
							}
						}}
						options={RETENTION_MODES.map((mode) => ({
							value: mode,
							label: retentionTitle(mode, retention.days),
						}))}
						size="sm"
						value={retention.mode}
					/>
				</>
			}
			description={
				retention.mode === "keepForever" && keptForeverCount !== undefined
					? `${keptForeverCount} ${keptForeverCount === 1 ? "recording" : "recordings"} kept.`
					: undefined
			}
			label="Keep recordings"
		/>
	);
}

/**
 * Recording: the two recording permissions, the input device, the
 * recordings folder with its disk usage, and the "Keep recordings" rule.
 */
export function RecordingSection() {
	const client = useBridge();
	const recording = useSnapshot("settings.recording");
	if (!recording) {
		return <SectionPage id="recording" />;
	}
	const allGranted = recording.permissions.every(
		(permission) => permission.state === "granted",
	);

	return (
		<SectionPage
			error={recording.error}
			errorDetails={recording.errorDetails}
			id="recording"
		>
			<FormCard
				footer={
					allGranted
						? "Steno can record your microphone and the audio of your calls."
						: undefined
				}
				title="Permissions"
			>
				{recording.permissions.map((permission) => (
					<PermissionRow
						isRequesting={permission.isRequesting}
						key={permission.kind}
						kind={permission.kind}
						onOpenSystemSettings={() =>
							send(client, "system.openSystemSettings", {
								kind: permission.kind,
							})
						}
						onRequest={() =>
							send(client, "settings.recording.requestPermission", {
								kind: permission.kind,
							})
						}
						state={permission.state}
					/>
				))}
			</FormCard>

			<FormCard title="Microphone">
				<FormRow
					control={
						<>
							<Select
								aria-label="Microphone"
								className="w-56"
								data-testid="input-device"
								onValueChange={(value) =>
									send(client, "settings.recording.setInputDevice", {
										value:
											value === null || value === DEFAULT_INPUT ? "" : value,
									})
								}
								options={[
									{ value: DEFAULT_INPUT, label: "System default" },
									...recording.devices.map((device) => ({
										value: device.uid,
										label: device.name,
									})),
								]}
								size="sm"
								value={recording.inputDeviceUID ?? DEFAULT_INPUT}
							/>
							<Button
								aria-label="Refresh microphones"
								data-testid="refresh-devices"
								onClick={() =>
									send(client, "settings.recording.refreshDevices")
								}
								size="icon-sm"
								variant="ghost"
							>
								<RefreshCwIcon aria-hidden="true" />
							</Button>
						</>
					}
					label="Microphone"
				/>
			</FormCard>

			<FormCard
				footer={`${recording.retentionFootnote} Short voice samples stay until you have named the speaker.`}
				title="Recordings"
			>
				<FormRow
					control={
						<>
							<FormValue
								data-testid="audio-folder"
								icon={<FolderIcon aria-hidden="true" />}
								title={recording.audioFolderPath}
							>
								{recording.audioFolderName}
							</FormValue>
							<Button
								data-testid="reveal-folder"
								onClick={() => send(client, "settings.recording.revealFolder")}
								size="sm"
								variant="ghost"
							>
								Show in Finder
							</Button>
							<Button
								data-testid="choose-folder"
								onClick={() => send(client, "settings.recording.chooseFolder")}
								size="sm"
								variant="outline"
							>
								Choose…
							</Button>
						</>
					}
					description={
						<span data-testid="folder-usage">{folderUsageText(recording)}</span>
					}
					label="Folder"
				/>
				<RetentionRow
					keptForeverCount={recording.keptForeverCount}
					retention={recording.retention}
				/>
			</FormCard>
		</SectionPage>
	);
}
