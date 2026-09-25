import { stenoLink } from "@modules/steno-link/native";
import { useNavigation } from "@react-navigation/native";
import type { NativeStackNavigationProp } from "@react-navigation/native-stack";
import { File } from "expo-file-system";
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
import { CHUNK_SIZE, recordingFileName } from "./recording-options";
import { applyRecovery, planRecovery } from "./recovery";
import { useRecorder } from "./use-recorder";

/** The "Today" / "Yesterday" labels refresh once a minute. */
const CLOCK_TICK_MS = 60_000;

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

	const recorder = useRecorder({
		onStarted: (session) => {
			void update((current) =>
				addRecording(
					current,
					{
						recordingID: session.recordingID,
						fileName: recordingFileName(session.recordingID),
						sourceUri: session.sourceUri,
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
					? patchRecording(current, recordingID, { ...fields, sourceUri: null })
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

	// Rows left in `recording` by a crash: move the recorder's file into the
	// queue and queue it, or mark the row failed when nothing survived.
	const recoveredOnce = useRef(false);
	useEffect(() => {
		if (!ready || recoveredOnce.current) return;
		recoveredOnce.current = true;
		void planRecovery(index, {
			size: (fileName) => {
				const file = queuedFile(fileName);
				return file.exists ? file.size : 0;
			},
			adopt: (sourceUri, fileName) =>
				new File(sourceUri).move(queuedFile(fileName), { overwrite: true }),
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
		const timer = setInterval(() => setNow(new Date()), CLOCK_TICK_MS);
		return () => clearInterval(timer);
	}, []);

	const toggle = useCallback(async () => {
		setError(null);
		setBusy(true);
		try {
			if (recorder.isRecording) {
				await recorder.stop();
			} else {
				await recorder.start();
			}
		} catch (caught) {
			setError(errorMessage(caught));
		} finally {
			// `onFinished` / `onFailed` clear it too; this covers a stop() that
			// found nothing to stop.
			setBusy(false);
		}
	}, [recorder]);

	const macLabel = pairing ? pairing.mac.macName : "Pair a Mac";
	const statusLine = recorder.isRecording
		? recorder.paused
			? `Paused at ${formatDuration(recorder.elapsedSeconds)}`
			: formatDuration(recorder.elapsedSeconds)
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
				{recorder.paused ? (
					<View className="items-center gap-2">
						<AppText className="text-center" variant="muted">
							A call or another app paused the recording. Resume, or stop to
							keep what you have.
						</AppText>
						<PressableScale
							accessibilityLabel="Resume recording"
							accessibilityRole="button"
							hitSlop={HIT_SLOP}
							onPress={recorder.resume}
						>
							<AppText variant="heading">Resume</AppText>
						</PressableScale>
					</View>
				) : null}
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
		case "idle":
			return macName
				? reachable
					? `${macName} is nearby.`
					: `Paired with ${macName}.`
				: "Ready to record.";
	}
}
