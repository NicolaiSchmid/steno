import { FlatList, View } from "react-native";

import { AppText } from "@/components/AppText";
import { PressableScale } from "@/components/PressableScale";
import type { QueuedRecording } from "@/features/queue/queue-index";
import { HIT_SLOP } from "@/lib/motion";
import {
	formatBytes,
	formatDuration,
	formatStartedAt,
	uploadPercent,
} from "./format";
import { SyncStatusBadge } from "./SyncStatusBadge";

/**
 * Every recording on the phone, newest first, with its sync state. A failed
 * row gets a Retry action and shows the last error; an uploading row shows
 * the percentage from uploaded chunks plus the bytes in flight.
 */
export function RecordingList({
	recordings,
	progress,
	now,
	onRetry,
}: {
	recordings: QueuedRecording[];
	progress: Readonly<Record<string, number>>;
	now: Date;
	onRetry: (recordingID: string) => void;
}) {
	const sorted = [...recordings].sort((a, b) =>
		b.startedAt.localeCompare(a.startedAt),
	);

	if (sorted.length === 0) {
		return (
			<View className="flex-1 items-center justify-center px-6">
				<AppText className="text-center" variant="muted">
					No recordings yet. Tap the button to start one; it stays on this phone
					until your Mac picks it up.
				</AppText>
			</View>
		);
	}

	return (
		<FlatList
			className="flex-1"
			contentContainerClassName="gap-2 px-6 pb-8"
			data={sorted}
			keyExtractor={(item) => item.recordingID}
			renderItem={({ item }) => (
				<RecordingRow
					inFlightBytes={progress[item.recordingID] ?? 0}
					now={now}
					onRetry={onRetry}
					recording={item}
				/>
			)}
		/>
	);
}

function RecordingRow({
	recording,
	inFlightBytes,
	now,
	onRetry,
}: {
	recording: QueuedRecording;
	inFlightBytes: number;
	now: Date;
	onRetry: (recordingID: string) => void;
}) {
	const detail =
		recording.state === "uploading"
			? `Uploading ${uploadPercent(
					recording.byteCount,
					recording.uploadedChunks.length,
					recording.chunkSize,
					inFlightBytes,
				)}%`
			: undefined;

	return (
		<View className="gap-2 rounded-2xl border border-border bg-card p-4">
			<View className="flex-row items-start justify-between gap-3">
				<View className="flex-1 gap-0.5">
					<AppText variant="heading">
						{formatStartedAt(recording.startedAt, now)}
					</AppText>
					<AppText variant="meta">
						{formatDuration(recording.durationSeconds)}
						{recording.byteCount > 0
							? ` · ${formatBytes(recording.byteCount)}`
							: ""}
					</AppText>
				</View>
				<SyncStatusBadge detail={detail} state={recording.state} />
			</View>
			{recording.state === "failed" || recording.lastError ? (
				<View className="flex-row items-center justify-between gap-3">
					<AppText className="flex-1" variant="error">
						{recording.lastError ?? "Upload failed"}
					</AppText>
					{recording.state === "failed" ? (
						<PressableScale
							accessibilityLabel="Retry upload"
							accessibilityRole="button"
							hitSlop={HIT_SLOP}
							onPress={() => onRetry(recording.recordingID)}
						>
							<AppText variant="heading">Retry</AppText>
						</PressableScale>
					) : null}
				</View>
			) : null}
		</View>
	);
}
