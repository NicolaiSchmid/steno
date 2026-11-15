import { CheckCircle2Icon, CircleIcon, XCircleIcon } from "lucide-react";
import type { ReactNode } from "react";
import type { RecordingSettingsSnapshot } from "@/bridge/contract";
import { Badge, Button, FormRow } from "@/components/ui";
import { type PlatformWords, usePlatform } from "@/lib/platform";

type Permission = RecordingSettingsSnapshot["permissions"][number];
export type PermissionKind = Permission["kind"];
export type PermissionState = Permission["state"];

/**
 * The permission's name and what Steno does with it, the onboarding's
 * words, with who asks and when where the platform asks at all.
 */
export function permissionCopy(
	kind: PermissionKind,
	{ computer, systemAudioPrompt, localNetworkPrompt }: PlatformWords,
): {
	title: string;
	explanation: string;
} {
	const asks = (prompt: string | undefined) => (prompt ? `${prompt} ` : "");
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
				explanation: `Steno records the other side from the apps playing audio on this ${computer}. ${asks(systemAudioPrompt)}Required.`,
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
				explanation: `The Steno iPhone app sends recordings over your Wi-Fi. ${asks(localNetworkPrompt)}Optional.`,
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
	/**
	 * Offers "Check again" once denied, beside "Open System Settings" where
	 * the platform has a place Steno can open.
	 */
	onCheckAgain?: (() => void) | undefined;
	/**
	 * The permission is granted by the system later, not here (the local
	 * network prompt comes with the first pairing): the one action is "Got
	 * it", which skips the step.
	 */
	acknowledgeOnly?: boolean;
	/**
	 * Onboarding's compact row. Its action is a primary button while the row
	 * `leads` (the first required step not yet allowed), an outline one
	 * otherwise, so the page shows one primary at a time.
	 */
	prominent?: boolean;
	/** The page's next step (see `prominent`); true unless the page says. */
	leads?: boolean;
	/**
	 * Another row's request is in flight: the host handles one at a time, so
	 * every Allow waits while one runs (the test recording takes 30 s).
	 */
	requestDisabled?: boolean;
	/**
	 * The host did not run the system audio test, or stopped it, because a
	 * recording is starting, running or stopping: the row says why.
	 */
	waitsForRecording?: boolean | undefined;
}

/** Why the system audio test did not run while Steno records. */
export const TONE_TEST_WAITS =
	"Stop the recording first. The test plays a short tone, and the recording would pick it up.";

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
	leads = true,
	requestDisabled = false,
	waitsForRecording = false,
}: PermissionRowProps) {
	const { words } = usePlatform();
	const copy = permissionCopy(kind, words);
	let description: string | undefined;
	if (state !== "granted" && !skipped) {
		// The Optional badge says it already; the sentence need not repeat it.
		description = optional
			? copy.explanation.replace(/ Optional\.$/, "")
			: copy.explanation;
	}
	// Off the Mac the system says nothing before a recording opens the
	// device, and Steno has no prompt to raise.
	const untilRecording =
		state === "unknown" && !acknowledgeOnly
			? words.unknownUntilRecording
			: undefined;
	if (untilRecording && description) {
		description = `${description} ${untilRecording}`;
	}
	if (isRequesting && kind === "systemAudio") {
		description = "Listening for the test tone, up to 30 seconds…";
	} else if (waitsForRecording && state !== "granted") {
		description = TONE_TEST_WAITS;
	}
	const action = prominent && leads && !optional ? "primary" : "outline";
	const openSettings = (variant: typeof action) => (
		<Button
			data-testid={`permission-${kind}-open`}
			onClick={onOpenSystemSettings}
			size="sm"
			variant={variant}
		>
			Open {words.systemSettings}
		</Button>
	);

	let control: ReactNode;
	// A badge alone stays beside the title; buttons go under the text when narrow.
	let statusOnly = true;
	if (state === "granted") {
		control = <Badge variant="success">Allowed</Badge>;
	} else if (skipped) {
		control = <Badge data-testid={`permission-${kind}-skipped`}>Skipped</Badge>;
	} else if (state === "denied") {
		statusOnly = !words.opensSystemSettings && !onCheckAgain;
		control = (
			<>
				{words.opensSystemSettings ? (
					openSettings(action)
				) : (
					<Badge variant="warning">Not allowed</Badge>
				)}
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
	} else if (untilRecording) {
		statusOnly = !words.opensSystemSettings;
		control = words.opensSystemSettings ? (
			openSettings("outline")
		) : (
			<Badge data-testid={`permission-${kind}-at-recording`}>
				Not checked yet
			</Badge>
		);
	} else if (acknowledgeOnly && onSkip) {
		statusOnly = false;
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
		statusOnly = false;
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
			compact={prominent}
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
			statusOnly={statusOnly}
			tone={
				state === "granted"
					? "success"
					: state === "denied"
						? "warning"
						: "muted"
			}
		/>
	);
}
