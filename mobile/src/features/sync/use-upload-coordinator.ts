import { stenoLink } from "@modules/steno-link";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AppState } from "react-native";

import {
	macRegistry,
	restartBrowsing,
	useMacDiscovery,
} from "@/features/discovery/use-mac-discovery";
import { usePairing } from "@/features/pairing/PairingProvider";
import { HandoverError, macOrigin } from "@/features/pairing/pairing-client";
import { deviceIdentity } from "@/features/pairing/pairing-store";
import { useQueue } from "@/features/queue/QueueProvider";
import {
	deleteQueuedFile,
	queuedFileExists,
	queuedFileUri,
} from "@/features/queue/queue-files";
import {
	chunkPlan,
	findRecording,
	isPending,
	markChunk,
	type QueueIndex,
	resetForUpload,
	type SyncState,
	scheduleRetry,
	setState,
	syncChunks,
} from "@/features/queue/queue-index";
import {
	announce,
	complete,
	status as fetchStatus,
	metadataFor,
	type Session,
	startChunkUpload,
} from "./recording-client";
import {
	type Action,
	backoffMs,
	parseChunkTaskID,
	planNext,
	taskIDs,
} from "./upload-coordinator";

/**
 * Executes the planner (plan P6): resolves the paired Mac when Bonjour sees
 * it, runs one action per tick, feeds results back into the queue, retries on
 * the backoff, reconciles with the background session after a relaunch, and
 * turns a 401 into `unpaired` for everything pending.
 */
export type CoordinatorStatus = SyncState | "searching" | "idle";

export type UploadCoordinator = {
	status: CoordinatorStatus;
	macName: string | null;
	reachable: boolean;
	/** Bytes sent of the chunk in flight, per recording id. */
	progress: Readonly<Record<string, number>>;
	retryNow(recordingID?: string): void;
};

function errorMessage(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

function unpairAll(index: QueueIndex): QueueIndex {
	return index.recordings
		.filter(isPending)
		.reduce((acc, r) => setState(acc, r.recordingID, "unpaired"), index);
}

export function useUploadCoordinator(): UploadCoordinator {
	const { pairing, ready: pairingReady, clear: clearPairing } = usePairing();
	const { index, ready: queueReady, update } = useQueue();
	const discovery = useMacDiscovery(pairingReady && pairing !== null);
	const [session, setSession] = useState<Session | null>(null);
	const [progress, setProgress] = useState<Record<string, number>>({});
	const inFlight = useRef(new Set<string>());
	const ticking = useRef(false);
	const rerun = useRef(false);
	const waitTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
	const indexRef = useRef(index);
	const sessionRef = useRef(session);

	const service = pairing
		? (discovery.services.find(
				(s) => s.macID?.toLowerCase() === pairing.mac.macID.toLowerCase(),
			) ?? null)
		: null;
	const serviceName = service?.name ?? null;

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

	const handleUnauthorized = useCallback(async () => {
		await update(unpairAll);
		await clearPairing();
	}, [update, clearPairing]);

	const fail = useCallback(
		async (recordingID: string, error: unknown) => {
			if (error instanceof HandoverError && error.kind === "unauthorized") {
				await handleUnauthorized();
				return;
			}
			await update((current) => {
				const rec = findRecording(current, recordingID);
				if (!rec || !isPending(rec)) return current;
				return scheduleRetry(
					current,
					recordingID,
					new Date(),
					backoffMs(rec.attempts + 1, Math.random),
					errorMessage(error),
				);
			});
		},
		[update, handleUnauthorized],
	);

	const execute = useCallback(
		async (action: Action, active: Session) => {
			switch (action.kind) {
				case "announce": {
					const rec = findRecording(indexRef.current, action.recordingID);
					if (!rec) return;
					const id = taskIDs.announce(rec.recordingID);
					inFlight.current.add(id);
					try {
						if (!queuedFileExists(rec.fileName)) {
							await update((current) =>
								setState(current, rec.recordingID, "failed", {
									lastError: "The recording file is missing",
								}),
							);
							return;
						}
						const { deviceName } = await deviceIdentity();
						const result = await announce(active, metadataFor(rec, deviceName));
						await update((current) =>
							setState(
								syncChunks(current, rec.recordingID, result.receivedChunks),
								rec.recordingID,
								"uploading",
								{ lastError: null },
							),
						);
					} catch (error) {
						await fail(rec.recordingID, error);
					} finally {
						inFlight.current.delete(id);
					}
					return;
				}
				case "upload-chunk": {
					const rec = findRecording(indexRef.current, action.recordingID);
					if (!rec) return;
					const chunk = chunkPlan(rec.byteCount, rec.chunkSize)[action.chunk];
					if (!chunk) return;
					const id = taskIDs.chunk(rec.recordingID, action.chunk);
					inFlight.current.add(id);
					try {
						await startChunkUpload(
							active,
							rec.recordingID,
							chunk,
							queuedFileUri(rec.fileName),
						);
					} catch (error) {
						inFlight.current.delete(id);
						await fail(rec.recordingID, error);
					}
					return;
				}
				case "complete": {
					const rec = findRecording(indexRef.current, action.recordingID);
					if (!rec) return;
					const id = taskIDs.complete(rec.recordingID);
					inFlight.current.add(id);
					try {
						const result = await complete(active, rec.recordingID);
						if (result.kind === "complete") {
							await update((current) =>
								setState(current, rec.recordingID, "delivered", {
									meetingID: result.meetingID,
									lastError: null,
								}),
							);
							deleteQueuedFile(rec.fileName);
						} else if (result.kind === "missing-chunks") {
							const remote = await fetchStatus(active, rec.recordingID);
							await update((current) =>
								syncChunks(current, rec.recordingID, remote.receivedChunks),
							);
						} else {
							await update((current) =>
								setState(
									syncChunks(current, rec.recordingID, []),
									rec.recordingID,
									"failed",
									{ lastError: "The Mac received a damaged file" },
								),
							);
						}
					} catch (error) {
						await fail(rec.recordingID, error);
					} finally {
						inFlight.current.delete(id);
					}
					return;
				}
				case "wait":
				case "idle":
					return;
			}
		},
		[update, fail],
	);

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
				{ reachable: active !== null, serviceName },
				inFlight.current,
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
			await execute(action, active);
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
	}, [queueReady, serviceName, execute]);

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
			link.addListener("uploadFinished", ({ taskID, status }) => {
				inFlight.current.delete(taskID);
				const parsed = parseChunkTaskID(taskID);
				if (!parsed) return;
				const { recordingID, chunk } = parsed;
				setProgress((p) => {
					const { [recordingID]: _done, ...rest } = p;
					return rest;
				});
				const apply = async () => {
					if (status === 204 || status === 200) {
						await update((current) =>
							findRecording(current, recordingID)
								? markChunk(current, recordingID, chunk)
								: current,
						);
					} else if (status === 401) {
						await handleUnauthorized();
					} else if (status === 404) {
						// The Mac lost the partial (restart, cleanup): announce again.
						await update((current) => {
							const rec = findRecording(current, recordingID);
							if (rec?.state !== "uploading") return current;
							return setState(
								syncChunks(current, recordingID, []),
								recordingID,
								"queued",
								{
									lastError: "The Mac forgot the upload; starting over",
								},
							);
						});
					} else {
						await fail(
							recordingID,
							new HandoverError(
								"server",
								status,
								`Chunk ${chunk} was answered ${status}`,
							),
						);
					}
				};
				void apply().finally(tick);
			}),
			link.addListener("uploadFailed", ({ taskID, message, retryable }) => {
				inFlight.current.delete(taskID);
				const parsed = parseChunkTaskID(taskID);
				if (!parsed || !retryable) {
					tick();
					return;
				}
				void fail(parsed.recordingID, new Error(message)).finally(tick);
			}),
		];
		return () => {
			for (const sub of subs) sub.remove();
		};
	}, [update, fail, handleUnauthorized, tick]);

	// After launch or foreground: remember what iOS kept running, refresh
	// browsing, and re-plan. Chunks that finished while JS was dead are
	// recovered from the Mac's status at the next announce (200 + chunks).
	useEffect(() => {
		const reconcile = async () => {
			try {
				const pending = await stenoLink().pendingUploads();
				for (const id of pending) inFlight.current.add(id);
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
	}, [tick]);

	// Chunks that the session reports as in flight but whose announce never
	// happened in this JS lifetime still count; re-announcing is idempotent.
	useEffect(() => {
		if (!session) return;
		let cancelled = false;
		const refresh = async () => {
			for (const rec of indexRef.current.recordings) {
				if (rec.state !== "uploading" || cancelled) continue;
				try {
					const remote = await fetchStatus(session, rec.recordingID);
					await update((current) =>
						findRecording(current, rec.recordingID)?.state === "uploading"
							? syncChunks(current, rec.recordingID, remote.receivedChunks)
							: current,
					);
				} catch (error) {
					if (error instanceof HandoverError && error.kind === "not-found") {
						await update((current) =>
							findRecording(current, rec.recordingID)?.state === "uploading"
								? setState(
										syncChunks(current, rec.recordingID, []),
										rec.recordingID,
										"queued",
									)
								: current,
						);
					} else if (
						error instanceof HandoverError &&
						error.kind === "unauthorized"
					) {
						await handleUnauthorized();
						return;
					}
				}
			}
		};
		void refresh().finally(tick);
		return () => {
			cancelled = true;
		};
	}, [session, update, handleUnauthorized, tick]);

	const retryNow = useCallback(
		(recordingID?: string) => {
			void update((current) => {
				const targets = current.recordings.filter(
					(r) =>
						(recordingID ? r.recordingID === recordingID : true) &&
						(r.state === "failed" ||
							r.state === "queued" ||
							r.state === "uploading"),
				);
				return targets.reduce(
					(acc, r) => resetForUpload(acc, r.recordingID),
					current,
				);
			}).then(() => {
				restartBrowsing();
				macRegistry.apply({ type: "reset" });
				tick();
			});
		},
		[update, tick],
	);

	const status = useMemo<CoordinatorStatus>(() => {
		if (!pairing) return "unpaired";
		const rows = index.recordings;
		if (rows.some((r) => r.state === "uploading") && session)
			return "uploading";
		if (rows.some(isPending)) return session ? "queued" : "searching";
		if (rows.some((r) => r.state === "failed")) return "failed";
		return "idle";
	}, [pairing, index, session]);

	return {
		status,
		macName: pairing?.mac.macName ?? null,
		reachable: session !== null,
		progress,
		retryNow,
	};
}
