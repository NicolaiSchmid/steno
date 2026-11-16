import { CircleAlertIcon, TimerIcon } from "lucide-react";
import { useContext } from "react";
import type { RecordingSnapshot } from "@/bridge/contract";
import { send, useBridge, useSnapshot } from "@/bridge/hooks";
import { Button, Callout, focusRingClass, RecordMark } from "@/components/ui";
import { cn } from "@/lib/cn";
import { useCountdownSeconds, useElapsedSeconds } from "@/lib/use-now";
import { format } from "./format";
import { NarrowLayoutContext } from "./narrow-layout";

type NoticeSize = "sm" | "md";

/**
 * The recorder is about to stop on its own (the call ended): the seconds
 * left, counted down from the snapshot's figure, and a way to keep going.
 * It asks the user to decide, so it is a warning (amber), not live red. A
 * screen reader hears the impending stop once; the ticking seconds stay
 * out of the live region.
 */
function AutoStopNotice({
	autoStop,
	size,
}: {
	autoStop: NonNullable<RecordingSnapshot["autoStop"]>;
	size: NoticeSize;
}) {
	const client = useBridge();
	// The host's figure is taken as of the moment it arrived; the page counts
	// down from there until a snapshot with a new figure resets it.
	const remaining = useCountdownSeconds(autoStop.remainingSeconds);
	const reason = /[.!?]$/.test(autoStop.reason)
		? autoStop.reason
		: `${autoStop.reason}.`;
	const countdown = (
		<span className="font-mono tabular-nums">
			{format.countdown(remaining)}
		</span>
	);
	return (
		<Callout
			actions={
				<Button
					data-testid="keep-recording"
					onClick={() => send(client, "recording.keepGoing")}
					size="sm"
					variant="outline"
				>
					Keep recording
				</Button>
			}
			announcement={`Recording stops on its own soon. ${reason} Choose Keep recording to go on.`}
			data-testid="auto-stop"
			description={reason}
			icon={<TimerIcon aria-hidden="true" />}
			size={size}
			title={<>Stops in {countdown}</>}
			variant="warning"
		/>
	);
}

/** A warning or error from the recorder, kept until the user dismisses it. */
function RecorderMessage({
	message,
	kind,
	size,
}: {
	message: string;
	kind: "warning" | "error";
	size: NoticeSize;
}) {
	const client = useBridge();
	return (
		<Callout
			actions={
				<Button
					data-testid="dismiss-recorder-message"
					onClick={() => send(client, "recording.clearMessages")}
					size="sm"
					variant="ghost"
				>
					Dismiss
				</Button>
			}
			data-testid={`recorder-${kind}`}
			icon={<CircleAlertIcon aria-hidden="true" />}
			size={size}
			title={message}
			variant={kind === "error" ? "destructive" : "warning"}
		/>
	);
}

/**
 * What the recorder has to say that cannot wait: the auto-stop countdown
 * and a warning or error. Under the Record control in the sidebar (`sm`),
 * and in the narrow window's recorder bar (`md`).
 */
export function RecorderNotices({
	recording,
	size,
}: {
	recording: RecordingSnapshot | undefined;
	size: NoticeSize;
}) {
	return (
		<>
			{recording?.autoStop && recording.state === "recording" ? (
				<AutoStopNotice autoStop={recording.autoStop} size={size} />
			) : null}
			{recording?.error ? (
				<RecorderMessage kind="error" message={recording.error} size={size} />
			) : recording?.warning ? (
				<RecorderMessage
					kind="warning"
					message={recording.warning}
					size={size}
				/>
			) : null}
		</>
	);
}

const ACTIVE_LABEL = {
	starting: "Starting…",
	recording: "Recording",
	stopping: "Stopping…",
} as const;

/**
 * The narrow window's recorder bar, under the reading column's header row:
 * while Steno records, the live mark, the elapsed time and Stop, whichever
 * meeting is open, with the recording meeting's title (a link that opens
 * it) when another one is; and the recorder's notices. The sidebar holds these in a
 * wide window, but in a narrow one it sits in the closed drawer. Nothing
 * when the window is wide or the recorder has nothing to say.
 */
export function RecorderBar() {
	const { narrow } = useContext(NarrowLayoutContext);
	const client = useBridge();
	const recording = useSnapshot("recording");
	const list = useSnapshot("meetings.list");
	const state = recording?.state ?? "idle";
	const active =
		state === "starting" || state === "recording" || state === "stopping";
	const elapsed = useElapsedSeconds(
		state === "recording" ? recording?.startedAt : undefined,
	);
	const notices =
		(recording?.autoStop !== undefined && state === "recording") ||
		Boolean(recording?.error ?? recording?.warning);
	// Another meeting is open: the bar names the one that records, as a
	// link to it, so Stop says what it stops.
	const elsewhere =
		recording?.meetingID !== undefined &&
		list?.selection !== recording.meetingID
			? list?.groups
					.flatMap((group) => group.meetings)
					.find((meeting) => meeting.id === recording.meetingID)
			: undefined;
	if (!narrow || !recording || (!active && !notices)) {
		return null;
	}
	return (
		<div
			className="flex shrink-0 flex-col gap-2 border-border border-b px-5 pb-3"
			data-testid="recorder-bar"
		>
			{active ? (
				<div className="flex min-w-0 items-center gap-3">
					<RecordMark pulse={state === "recording"} />
					<span className="min-w-0 flex-1 truncate text-foreground text-label">
						{ACTIVE_LABEL[state]}
						{elsewhere ? (
							<>
								{" "}
								<button
									className={cn(
										"rounded-xs font-normal text-foreground underline underline-offset-2 hover:no-underline",
										focusRingClass,
									)}
									data-testid="bar-open-recording"
									onClick={() =>
										send(client, "meetings.select", {
											meetingID: elsewhere.id,
										})
									}
									type="button"
								>
									{elsewhere.title}
								</button>
							</>
						) : null}
						{state === "recording" ? (
							<span className="ml-2 font-mono font-normal text-meta text-muted-foreground tabular-nums">
								{format.duration(elapsed)}
							</span>
						) : null}
					</span>
					<Button
						data-testid="bar-stop"
						disabled={state !== "recording"}
						onClick={() => send(client, "recording.stop")}
						size="sm"
						variant="primary"
					>
						Stop
					</Button>
				</div>
			) : null}
			<RecorderNotices recording={recording} size="md" />
		</div>
	);
}
