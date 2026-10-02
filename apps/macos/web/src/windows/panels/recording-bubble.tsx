import { SquareIcon } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { RecordingSnapshot } from "@/bridge/contract";
import { send, useBridge, usePageReady, useSnapshot } from "@/bridge/hooks";
import { Button, RecordMark } from "@/components/ui";
import { cn } from "@/lib/cn";
import { format } from "@/windows/main/format";
import { useCountdownSeconds, useElapsedSeconds } from "@/windows/main/use-now";
import {
	CountdownHairline,
	EMPTY_HISTORY,
	LiveBars,
	PanelBar,
	pushLevel,
} from "./panel-bar";
import { useReportSize } from "./panel-shell";

/**
 * The recording bubble (`#/panel/bubble`): the Swift `RecordingBubbleView`
 * over the `recording` snapshot. Shown by the shell for every recorder
 * state but idle. The body opens the live meeting in the main window; the
 * square stops the recording; while the auto-stop is armed, a second row
 * says when and offers "Keep recording" over the draining hairline.
 */

/** What the bubble renders for a recorder state (`BubblePresentation`). */
export interface BubblePresentation {
	/** "Starting…" or "Stopping…"; undefined while recording (the clock shows). */
	text?: string;
	showsBars: boolean;
	showsStop: boolean;
	stopEnabled: boolean;
	autoStop?: RecordingSnapshot["autoStop"];
}

export function presentBubble(
	snapshot: RecordingSnapshot | undefined,
): BubblePresentation {
	switch (snapshot?.state) {
		case "starting":
			return {
				text: "Starting…",
				showsBars: false,
				showsStop: false,
				stopEnabled: false,
			};
		case "recording":
			return {
				showsBars: true,
				showsStop: true,
				stopEnabled: true,
				...(snapshot.autoStop ? { autoStop: snapshot.autoStop } : {}),
			};
		case "stopping":
			return {
				text: "Stopping…",
				showsBars: false,
				showsStop: true,
				stopEnabled: false,
			};
		default:
			return { showsBars: false, showsStop: false, stopEnabled: false };
	}
}

/** The loudest lane as a bar fraction. */
export function levelFraction(
	level: RecordingSnapshot["level"] | undefined,
): number {
	if (!level) return 0;
	return Math.max(level.mic, level.system);
}

export function RecordingBubble() {
	const client = useBridge();
	usePageReady(client);
	const recording = useSnapshot("recording");
	const presentation = presentBubble(recording);
	const elapsed = useElapsedSeconds(
		recording?.state === "recording" ? recording.startedAt : undefined,
	);
	const [history, setHistory] = useState<readonly number[]>(EMPTY_HISTORY);
	useEffect(() => {
		if (recording?.state !== "recording") {
			setHistory(EMPTY_HISTORY);
			return;
		}
		setHistory((previous) =>
			pushLevel(previous, levelFraction(recording.level)),
		);
	}, [recording?.state, recording?.level]);
	const bar = useRef<HTMLDivElement>(null);
	useReportSize(bar);

	const open = () => {
		send(client, "window.open", {
			window: "main",
			...(recording?.meetingID ? { meetingID: recording.meetingID } : {}),
		});
	};

	return (
		<PanelBar
			className="relative max-w-[480px] flex-col items-stretch p-1.5"
			data-testid="recording-bubble"
			ref={bar}
		>
			<div className="flex min-h-7 items-center gap-2.5">
				<button
					aria-label={presentation.text ?? "Recording"}
					className="flex items-center gap-2.5 rounded-lg text-left outline-none focus-visible:ring-2 focus-visible:ring-ring"
					data-testid="bubble-open"
					onClick={open}
					title="Open the meeting in Steno"
					type="button"
				>
					<span className="flex size-5 items-center justify-center rounded-md bg-card">
						<RecordMark pulse={recording?.state === "recording"} />
					</span>
					{presentation.text ? (
						<span className="text-muted-foreground text-xs">
							{presentation.text}
						</span>
					) : (
						<>
							{presentation.showsBars ? <LiveBars history={history} /> : null}
							<span
								className="font-mono text-muted-foreground text-xs tabular-nums"
								data-testid="bubble-elapsed"
							>
								{format.duration(elapsed)}
							</span>
						</>
					)}
				</button>
				{presentation.autoStop ? (
					<Button
						className="ml-auto"
						data-testid="bubble-keep-recording"
						onClick={() => send(client, "recording.keepGoing")}
						size="sm"
						variant="outline"
					>
						Keep recording
					</Button>
				) : null}
				{presentation.showsStop ? (
					<Button
						aria-label="Stop recording"
						className={cn(!presentation.autoStop && "ml-auto")}
						data-testid="bubble-stop"
						disabled={!presentation.stopEnabled}
						onClick={() => send(client, "recording.stop")}
						size="icon-sm"
						title="Stop recording"
						variant="outline"
					>
						<SquareIcon className="fill-current text-destructive" />
					</Button>
				) : null}
			</div>
			{presentation.autoStop ? (
				<AutoStopLine autoStop={presentation.autoStop} />
			) : null}
		</PanelBar>
	);
}

/**
 * The armed auto-stop: "<reason>: stops in m:ss" over the hairline, counted
 * down from the snapshot's figure as the sidebar's notice counts.
 */
function AutoStopLine({
	autoStop,
}: {
	autoStop: NonNullable<RecordingSnapshot["autoStop"]>;
}) {
	const remaining = Math.max(0, useCountdownSeconds(autoStop.remainingSeconds));
	return (
		<>
			<p
				className="truncate px-1.5 pt-1 pb-2 text-foreground text-sm"
				data-testid="bubble-auto-stop"
			>
				{autoStop.reason.replace(/[.!?]$/, "")}: stops in{" "}
				<span className="font-mono tabular-nums">
					{format.countdown(remaining)}
				</span>
			</p>
			<CountdownHairline
				fractionRemaining={
					autoStop.totalSeconds > 0 ? remaining / autoStop.totalSeconds : 0
				}
			/>
		</>
	);
}
