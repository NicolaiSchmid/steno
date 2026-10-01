import { CheckCircle2Icon, CircleIcon, XCircleIcon } from "lucide-react";
import type { RecordingSettingsSnapshot } from "@/bridge/contract";
import { Badge, Button, FormRow } from "@/components/ui";

type Permission = RecordingSettingsSnapshot["permissions"][number];
export type PermissionKind = Permission["kind"];
export type PermissionState = Permission["state"];

/** The permission's name and what Steno does with it; the onboarding's words. */
export function permissionCopy(kind: PermissionKind): {
	title: string;
	explanation: string;
} {
	switch (kind) {
		case "microphone":
			return {
				title: "Microphone",
				explanation:
					"Steno records your side of a meeting from the microphone. Required.",
			};
		case "systemAudio":
			return {
				title: "System audio",
				explanation:
					"Steno records the other side from the apps playing audio on this Mac. macOS asks once, during a short test recording. Required.",
			};
		case "calendar":
			return {
				title: "Calendar",
				explanation:
					"Steno names recordings after the calendar event they overlap and suggests the attendees as speakers. Optional.",
			};
		case "localNetwork":
			return {
				title: "Local network",
				explanation:
					"The Steno iPhone app sends recordings over your Wi-Fi. macOS asks when you pair the first phone. Optional.",
			};
	}
}

export interface PermissionRowProps {
	kind: PermissionKind;
	state: PermissionState;
	isRequesting: boolean;
	onRequest: () => void;
	onOpenSystemSettings: () => void;
}

/**
 * One permission: its state glyph, title, the explanation while it is not
 * granted, and the action that fits the state.
 */
export function PermissionRow({
	kind,
	state,
	isRequesting,
	onRequest,
	onOpenSystemSettings,
}: PermissionRowProps) {
	const copy = permissionCopy(kind);
	let description: string | undefined;
	if (state !== "granted") {
		description = copy.explanation;
	}
	if (isRequesting && kind === "systemAudio") {
		description = "Listening for the test tone, up to 30 seconds…";
	}
	return (
		<FormRow
			control={
				state === "granted" ? (
					<Badge variant="live">Allowed</Badge>
				) : state === "denied" ? (
					<Button
						data-testid={`permission-${kind}-open`}
						onClick={onOpenSystemSettings}
						size="sm"
						variant="outline"
					>
						Open System Settings
					</Button>
				) : (
					<Button
						data-testid={`permission-${kind}-request`}
						disabled={isRequesting}
						onClick={onRequest}
						size="sm"
						variant="outline"
					>
						{isRequesting
							? "Asking…"
							: kind === "systemAudio"
								? "Run the test recording"
								: "Allow"}
					</Button>
				)
			}
			data-testid={`permission-${kind}`}
			description={description}
			icon={
				state === "granted" ? (
					<CheckCircle2Icon aria-hidden="true" />
				) : state === "denied" ? (
					<XCircleIcon aria-hidden="true" />
				) : (
					<CircleIcon aria-hidden="true" />
				)
			}
			label={copy.title}
			tone={
				state === "granted"
					? "primary"
					: state === "denied"
						? "warning"
						: "faint"
			}
		/>
	);
}
