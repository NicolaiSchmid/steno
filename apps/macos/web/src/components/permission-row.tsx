import { CheckCircle2Icon, CircleIcon, XCircleIcon } from "lucide-react";
import type { ReactNode } from "react";
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
	/** Shows the "Optional" badge beside the title (onboarding). */
	optional?: boolean;
	/** The step was skipped: no action, a "Skipped" badge. */
	skipped?: boolean;
	/** Offers a ghost "Skip" beside the action (optional steps in onboarding). */
	onSkip?: (() => void) | undefined;
	/** Offers "Check again" beside "Open System Settings" once denied. */
	onCheckAgain?: (() => void) | undefined;
	/**
	 * The permission is granted by macOS later, not here (the local network
	 * prompt comes with the first pairing): the one action is "Got it", which
	 * skips the step.
	 */
	acknowledgeOnly?: boolean;
	/** The row's action is the page's main one (onboarding): a primary button. */
	prominent?: boolean;
	/**
	 * Another row's request is in flight: the host handles one at a time, so
	 * every Allow waits while one runs (the test recording takes 30 s).
	 */
	requestDisabled?: boolean;
}

/**
 * One permission: its state glyph, title, the explanation while it is not
 * granted, and the action that fits the state. Settings and onboarding
 * share it; onboarding adds the optional badge, Skip and Check again.
 */
export function PermissionRow({
	kind,
	state,
	isRequesting,
	onRequest,
	onOpenSystemSettings,
	optional = false,
	skipped = false,
	onSkip,
	onCheckAgain,
	acknowledgeOnly = false,
	prominent = false,
	requestDisabled = false,
}: PermissionRowProps) {
	const copy = permissionCopy(kind);
	let description: string | undefined;
	if (state !== "granted" && !skipped) {
		description = copy.explanation;
	}
	if (isRequesting && kind === "systemAudio") {
		description = "Listening for the test tone, up to 30 seconds…";
	}
	const action = prominent && !optional ? "primary" : "outline";

	let control: ReactNode;
	if (state === "granted") {
		control = <Badge variant="live">Allowed</Badge>;
	} else if (skipped) {
		control = <Badge data-testid={`permission-${kind}-skipped`}>Skipped</Badge>;
	} else if (state === "denied") {
		control = (
			<>
				<Button
					data-testid={`permission-${kind}-open`}
					onClick={onOpenSystemSettings}
					size="sm"
					variant={action}
				>
					Open System Settings
				</Button>
				{onCheckAgain ? (
					<Button
						data-testid={`permission-${kind}-check`}
						onClick={onCheckAgain}
						size="sm"
						variant="outline"
					>
						Check again
					</Button>
				) : null}
			</>
		);
	} else if (acknowledgeOnly && onSkip) {
		control = (
			<Button
				data-testid={`permission-${kind}-skip`}
				onClick={onSkip}
				size="sm"
				variant="outline"
			>
				Got it
			</Button>
		);
	} else {
		control = (
			<>
				<Button
					data-testid={`permission-${kind}-request`}
					disabled={isRequesting || requestDisabled}
					onClick={onRequest}
					size="sm"
					variant={action}
				>
					{isRequesting
						? "Asking…"
						: kind === "systemAudio"
							? "Run the test recording"
							: "Allow"}
				</Button>
				{onSkip ? (
					<Button
						data-testid={`permission-${kind}-skip`}
						onClick={onSkip}
						size="sm"
						variant="ghost"
					>
						Skip
					</Button>
				) : null}
			</>
		);
	}

	return (
		<FormRow
			control={control}
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
			label={
				optional ? (
					<span className="inline-flex items-center gap-2">
						{copy.title}
						<Badge>Optional</Badge>
					</span>
				) : (
					copy.title
				)
			}
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
