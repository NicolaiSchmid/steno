import { stenoLink } from "@modules/steno-link";
import { useNavigation } from "@react-navigation/native";
import type { NativeStackNavigationProp } from "@react-navigation/native-stack";
import { useCallback, useEffect, useRef, useState } from "react";
import { View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { AppText } from "@/components/AppText";
import { PressableScale } from "@/components/PressableScale";
import { usePairing } from "@/features/pairing/PairingProvider";
import { useQueue } from "@/features/queue/QueueProvider";
import { queuedFile } from "@/features/queue/queue-files";
import {
	addRecording,
	findRecording,
	patchRecording,
	setState,
} from "@/features/queue/queue-index";
import { useRecorder } from "@/features/recording/recorder";
import {
	CHUNK_SIZE,
	recordingFileName,
} from "@/features/recording/recording-options";
import { applyRecovery, planRecovery } from "@/features/recording/recovery";
import {
	type CoordinatorStatus,
	useUploadCoordinator,
} from "@/features/sync/use-upload-coordinator";
import { errorMessage } from "@/lib/error-message";
import { HIT_SLOP } from "@/lib/motion";
import type { RootStackParamList } from "@/navigation/types";
import { formatDuration } from "./format";
import { RecordButton } from "./RecordButton";
import { RecordingList } from "./RecordingList";

/** The elapsed counter and "Today" labels refresh once a second. */
const CLOCK_TICK_MS = 1000;

/**
 * The one screen the scope gives the phone: record or stop, see every
 * recording with its sync state, open the pairing sheet.
 */
export function RecorderScreen() {
	const navigation =
		useNavigation<NativeStackNavigationProp<RootStackParamList>>();
	const { pairing } = usePairing();
	const { index, ready, update } = useQueue();
	const sync = useUploadCoordinator();
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const [now, setNow] = useState(() => new Date());
	const [elapsed, setElapsed] = useState(0);

	const recorder = useRecorder({
		onStarted: (session) => {
			void update((current) =>
				addRecording(
					current,
					{
						recordingID: session.recordingID,
						fileName: recordingFileName(session.recordingID),
						startedAt: session.startedAt.toISOString(),
						durationSeconds: 0,
						byteCount: 0,
						sha256: null,
						chunkSize: CHUNK_SIZE,
					},
					"recording",
				),
			);
		},
		onFinished: (finished) => {
			setBusy(false);
			void update((current) => {
				// The row normally exists from `onStarted`; add it if that write failed.
				const { recordingID, startedAt, ...fields } = finished;
				const patched = findRecording(current, recordingID)
					? patchRecording(current, recordingID, fields)
					: addRecording(
							current,
							{ recordingID, startedAt, ...fields, chunkSize: CHUNK_SIZE },
							"recording",
						);
				return setState(patched, recordingID, "queued");
			});
		},
		onFailed: (session, message) => {
			setBusy(false);
			setError(message);
			void update((current) =>
				findRecording(current, session.recordingID)
					? setState(current, session.recordingID, "failed", {
							lastError: message,
						})
					: current,
			);
		},
	});

	// Rows left in `recording` by a crash: queue the file if it exists.
	const recoveredOnce = useRef(false);
	useEffect(() => {
		if (!ready || recoveredOnce.current) return;
		recoveredOnce.current = true;
		void planRecovery(index, {
			size: (fileName) => {
				const file = queuedFile(fileName);
				return file.exists ? file.size : 0;
			},
			sha256: (fileName) => stenoLink().sha256(queuedFile(fileName).uri),
		})
			.then((patches) =>
				patches.length > 0
					? update((current) => applyRecovery(current, patches))
					: undefined,
			)
			.catch((caught) => console.warn("[recorder] recovery failed", caught));
	}, [ready, index, update]);

	useEffect(() => {
		const timer = setInterval(() => {
			setNow(new Date());
			setElapsed(recorder.elapsedSeconds());
		}, CLOCK_TICK_MS);
		return () => clearInterval(timer);
	}, [recorder]);

	const toggle = useCallback(async () => {
		setError(null);
		setBusy(true);
		try {
			if (recorder.isRecording) {
				await recorder.stop();
			} else {
				await recorder.start();
				setBusy(false);
			}
		} catch (caught) {
			setBusy(false);
			setError(errorMessage(caught));
		}
	}, [recorder]);

	const macLabel = pairing ? pairing.mac.macName : "Pair a Mac";
	const statusLine = recorder.isRecording
		? formatDuration(elapsed)
		: describeSync(sync.status, sync.macName, sync.reachable);

	return (
		<SafeAreaView className="flex-1 bg-background" edges={["top"]}>
			<View className="flex-row items-center justify-between px-6 pt-4 pb-2">
				<AppText variant="title">Steno</AppText>
				<PressableScale
					accessibilityHint="Opens the pairing sheet"
					accessibilityLabel={pairing ? `Paired with ${macLabel}` : macLabel}
					accessibilityRole="button"
					hitSlop={HIT_SLOP}
					onPress={() => navigation.navigate("Pairing")}
				>
					<View className="rounded-full border border-border bg-card px-3 py-1.5">
						<AppText variant="bodySm">{macLabel}</AppText>
					</View>
				</PressableScale>
			</View>

			<View className="items-center gap-4 px-6 py-6">
				<RecordButton
					busy={busy}
					onPress={() => void toggle()}
					recording={recorder.isRecording}
				/>
				<AppText
					accessibilityLiveRegion="polite"
					className="text-center"
					variant={recorder.isRecording ? "heading" : "muted"}
				>
					{statusLine}
				</AppText>
				{error ? (
					<AppText className="text-center" variant="error">
						{error}
					</AppText>
				) : null}
			</View>

			<RecordingList
				now={now}
				onRetry={sync.retryNow}
				progress={sync.progress}
				recordings={index.recordings}
			/>
		</SafeAreaView>
	);
}

function describeSync(
	status: CoordinatorStatus,
	macName: string | null,
	reachable: boolean,
): string {
	switch (status) {
		case "unpaired":
			return "Pair a Mac to hand over recordings.";
		case "searching":
			return macName ? `Looking for ${macName}…` : "Looking for your Mac…";
		case "uploading":
			return `Uploading to ${macName ?? "your Mac"}…`;
		case "queued":
			return reachable ? "Waiting to upload…" : "Waiting for your Mac…";
		case "failed":
			return "An upload failed. Retry from the list.";
		case "recording":
			return "Recording…";
		case "delivered":
		case "idle":
			return macName
				? reachable
					? `${macName} is nearby.`
					: `Paired with ${macName}.`
				: "Ready to record.";
	}
}
