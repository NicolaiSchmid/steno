import { macOrigin } from "@modules/steno-link";
import { stenoLink } from "@modules/steno-link/native";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AppState } from "react-native";

import { findByMacID } from "@/features/discovery/mac-registry";
import {
	restartBrowsing,
	useMacDiscovery,
} from "@/features/discovery/use-mac-discovery";
import { usePairing } from "@/features/pairing/PairingProvider";
import { deviceIdentity } from "@/features/pairing/pairing-store";
import { useQueue } from "@/features/queue/QueueProvider";
import { queuedFile } from "@/features/queue/queue-files";
import { findRecording, resetForUpload } from "@/features/queue/queue-index";
import {
	announce,
	complete,
	status as fetchStatus,
	type MacSession,
	startChunkUpload,
} from "./recording-client";
import {
	type CoordinatorStatus,
	coordinatorStatus,
	parseChunkTaskID,
	planNext,
} from "./upload-coordinator";
import { createUploadExecutor, type RecordingFiles } from "./upload-executor";

export type { CoordinatorStatus } from "./upload-coordinator";

/**
 * Runs the planner (plan P6) over the real module, files and clock: resolves
 * the paired Mac when Bonjour sees it, runs one action per tick through
 * `upload-executor.ts`, retries on the backoff, reconciles with the
 * background session after a relaunch, and turns a 401 into `unpaired`.
 */
export type UploadCoordinator = {
	status: CoordinatorStatus;
	macName: string | null;
	reachable: boolean;
	/** Bytes sent of the chunk in flight, per recording id. */
	progress: Readonly<Record<string, number>>;
	/** Puts a failed recording back in the queue, eligible now. */
	retryNow(recordingID: string): void;
};

const recordingFiles: RecordingFiles = {
	exists: (fileName) => queuedFile(fileName).exists,
	uri: (fileName) => queuedFile(fileName).uri,
	remove: (fileName) => queuedFile(fileName).delete(),
};

export function useUploadCoordinator(): UploadCoordinator {
	const { pairing, ready: pairingReady, clear: clearPairing } = usePairing();
	const { index, ready: queueReady, update } = useQueue();
	const discovery = useMacDiscovery(pairingReady && pairing !== null);
	const [session, setSession] = useState<MacSession | null>(null);
	const [progress, setProgress] = useState<Record<string, number>>({});
	const ticking = useRef(false);
	const rerun = useRef(false);
	const waitTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
	const indexRef = useRef(index);
	const sessionRef = useRef(session);

	const executor = useMemo(
		() =>
			createUploadExecutor({
				client: { announce, status: fetchStatus, complete, startChunkUpload },
				files: recordingFiles,
				deviceName: async () => (await deviceIdentity()).deviceName,
				update,
				onUnauthorized: clearPairing,
				now: () => new Date(),
				random: Math.random,
			}),
		[update, clearPairing],
	);

	const serviceName = pairing
		? (findByMacID(discovery.services, pairing.mac.macID)?.name ?? null)
		: null;

	// Resolve the Mac once per appearance; forget it when the service goes.
	useEffect(() => {
		if (!pairing || !serviceName) {
			setSession(null);
			return;
		}
		let cancelled = false;
		const { fingerprint } = pairing.mac;
		const token = pairing.token;
		stenoLink()
			.resolve(serviceName)
			.then((resolved) => {
				if (cancelled) return;
				setSession({
					endpoint: { origin: macOrigin(resolved), fingerprint },
					token,
				});
			})
			.catch((error) => {
				if (!cancelled) console.warn("[sync] resolve failed", error);
			});
		return () => {
			cancelled = true;
		};
	}, [pairing, serviceName]);

	const tick = useCallback(() => {
		if (!queueReady) return;
		if (ticking.current) {
			rerun.current = true;
			return;
		}
		ticking.current = true;
		if (waitTimer.current) {
			clearTimeout(waitTimer.current);
			waitTimer.current = null;
		}
		const run = async () => {
			const active = sessionRef.current;
			const action = planNext(
				indexRef.current,
				active !== null,
				executor.inFlight,
				new Date(),
			);
			if (action.kind === "wait") {
				const delay = Math.max(
					0,
					new Date(action.until).getTime() - Date.now(),
				);
				waitTimer.current = setTimeout(() => tick(), delay);
				return;
			}
			if (action.kind === "idle" || !active) return;
			await executor.execute(action, active, indexRef.current);
			rerun.current = true;
		};
		void run()
			.catch((error) => console.warn("[sync] tick failed", error))
			.finally(() => {
				ticking.current = false;
				if (rerun.current) {
					rerun.current = false;
					tick();
				}
			});
	}, [queueReady, executor]);

	// Publish the latest inputs to the tick loop and re-plan on every change.
	useEffect(() => {
		indexRef.current = index;
		sessionRef.current = session;
		tick();
	}, [tick, index, session]);

	// Background session events.
	useEffect(() => {
		const link = stenoLink();
		const subs = [
			link.addListener("uploadProgress", ({ taskID, bytesSent }) => {
				const parsed = parseChunkTaskID(taskID);
				if (!parsed) return;
				setProgress((p) => ({ ...p, [parsed.recordingID]: bytesSent }));
			}),
			link.addListener("uploadFinished", (event) => {
				const parsed = parseChunkTaskID(event.taskID);
				if (parsed) {
					setProgress((p) => {
						const { [parsed.recordingID]: _done, ...rest } = p;
						return rest;
					});
				}
				void executor.uploadFinished(event).finally(tick);
			}),
			link.addListener("uploadFailed", (event) => {
				void executor.uploadFailed(event).finally(tick);
			}),
		];
		return () => {
			for (const sub of subs) sub.remove();
		};
	}, [executor, tick]);

	// After launch or foreground: take the background session's word on which
	// chunks are in flight, refresh browsing, and re-plan. Chunks that
	// finished while JS was dead are recovered from the Mac's status at the
	// next announce (200 + chunks).
	useEffect(() => {
		const reconcile = async () => {
			try {
				executor.reconcile(await stenoLink().pendingUploads());
			} catch (error) {
				console.warn("[sync] pendingUploads failed", error);
			}
			tick();
		};
		void reconcile();
		const sub = AppState.addEventListener("change", (state) => {
			if (state === "active") {
				restartBrowsing();
				void reconcile();
			}
		});
		return () => sub.remove();
	}, [executor, tick]);

	// Chunks that the session reports as in flight but whose announce never
	// happened in this JS lifetime still count; re-announcing is idempotent.
	useEffect(() => {
		if (!session) return;
		let cancelled = false;
		void executor
			.refreshUploading(session, indexRef.current, () => cancelled)
			.finally(tick);
		return () => {
			cancelled = true;
		};
	}, [session, executor, tick]);

	const retryNow = useCallback(
		(recordingID: string) => {
			void update((current) =>
				findRecording(current, recordingID)?.state === "failed"
					? resetForUpload(current, recordingID)
					: current,
			).then(() => {
				restartBrowsing();
				tick();
			});
		},
		[update, tick],
	);

	const status = useMemo(
		() => coordinatorStatus(pairing !== null, index, session !== null),
		[pairing, index, session],
	);

	return {
		status,
		macName: pairing?.mac.macName ?? null,
		reachable: session !== null,
		progress,
		retryNow,
	};
}
