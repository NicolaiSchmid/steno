import { SquareIcon } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import appIcon from "@/assets/app-icon.png";
import type { RecordingSnapshot } from "@/bridge/contract";
import { send, useBridge, usePageReady, useSnapshot } from "@/bridge/hooks";
import { focusRingClass } from "@/components/ui";
import { cn } from "@/lib/cn";
import { useCountdownSeconds, useElapsedSeconds } from "@/lib/use-now";
import { format } from "@/windows/main/format";
import {
	CountdownHairline,
	EMPTY_HISTORY,
	LiveBars,
	PanelBar,
	pushLevel,
	softButtonClass,
} from "./panel-bar";
import { useClickOrDrag, useReportSize } from "./panel-shell";

/**
 * The recording bubble (`#/panel/bubble`), in the detection prompt's
 * language, over the `recording` snapshot: the app icon with a red dot
 * while live, the waveform and the clock, and a soft square Stop. Shown by the shell for every recorder
 * state but idle. The body opens the live meeting in the main window on a
 * click and drags the panel on a press that moves (`useClickOrDrag`); the
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

	const body = useClickOrDrag(open);

	return (
		<PanelBar
			className="relative max-w-[480px] flex-col items-stretch p-2"
			data-testid="recording-bubble"
			ref={bar}
		>
			<div className="flex items-center gap-2">
				<button
					{...body}
					aria-label={presentation.text ?? "Recording"}
					className={cn(
						"flex h-10 items-center gap-3 rounded-xl pr-2.5 pl-1.5 text-left transition-colors duration-(--duration-functional) ease-standard hover:bg-accent",
						focusRingClass,
					)}
					data-testid="bubble-open"
					title="Open the meeting in Steno"
					type="button"
				>
					<span className="relative shrink-0">
						<img
							alt=""
							className="size-7 rounded-md"
							draggable={false}
							height={28}
							src={appIcon}
							width={28}
						/>
						{recording?.state === "recording" ? (
							<span
								aria-hidden="true"
								className="absolute -top-0.5 -right-0.5 size-2.5 animate-status-pulse rounded-full bg-live ring-2 ring-popover"
								data-testid="bubble-live-dot"
							/>
						) : null}
					</span>
					{presentation.text ? (
						<span className="text-label text-muted-foreground">
							{presentation.text}
						</span>
					) : (
						<>
							{presentation.showsBars ? <LiveBars history={history} /> : null}
							<span
								className="font-semibold text-foreground text-reading tabular-nums"
								data-testid="bubble-elapsed"
							>
								{format.duration(elapsed)}
							</span>
						</>
					)}
				</button>
				{presentation.autoStop ? (
					<button
						className={cn(
							softButtonClass,
							"ml-auto h-10 px-3.5 font-medium text-label",
						)}
						data-testid="bubble-keep-recording"
						onClick={() => send(client, "recording.keepGoing")}
						type="button"
					>
						Keep recording
					</button>
				) : null}
				{presentation.showsStop ? (
					<button
						aria-label="Stop recording"
						className={cn(
							softButtonClass,
							"size-10",
							!presentation.autoStop && "ml-auto",
						)}
						data-testid="bubble-stop"
						disabled={!presentation.stopEnabled}
						onClick={() => send(client, "recording.stop")}
						title="Stop recording"
						type="button"
					>
						<SquareIcon className="size-3.5 fill-current text-live" />
					</button>
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
				className="truncate px-1.5 pt-1.5 pb-2.5 text-foreground text-label"
				data-testid="bubble-auto-stop"
			>
				{autoStop.reason.replace(/[.!?]$/, "")}: stops in{" "}
				<span className="tabular-nums">{format.countdown(remaining)}</span>
			</p>
			<CountdownHairline
				fractionRemaining={
					autoStop.totalSeconds > 0 ? remaining / autoStop.totalSeconds : 0
				}
			/>
		</>
	);
}
