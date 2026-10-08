import { macOrigin, paths } from "@modules/steno-link";
import {
	HandoverError,
	type MacEndpoint,
	pinnedRequest,
	stenoLink,
} from "@modules/steno-link/native";
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
import {
	canRetry,
	findRecording,
	isPending,
	resetForUpload,
} from "@/features/queue/queue-index";
import {
	announce,
	cancelAllUploads,
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
	taskIDs,
} from "./upload-coordinator";
import { createUploadExecutor, type RecordingFiles } from "./upload-executor";

export type { CoordinatorStatus } from "./upload-coordinator";

/**
 * Runs the planner (plan P6) over the real module, files and clock: resolves
 * the paired Mac when Bonjour sees it, and again after a request fails to
 * connect and on a timer while uploads are queued, runs one action per tick
 * through `upload-executor.ts`, retries on the backoff, reconciles with the
 * background session after a relaunch, and turns a 401 to the current
 * pairing's token into `unpaired`.
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

/** How soon a tick looks again while the pairing is changing. */
const REPAIRING_RECHECK_MS = 1000;

/**
 * How often the Mac is resolved again while uploads are queued and the app
 * is in the foreground. A new address that keeps the Bonjour name (a new
 * lease, Ethernet instead of Wi-Fi on one LAN) sends no event, and chunks out
 * to the old address fail no request, so only this finds it. A round takes
 * at most 20 s (the resolve and the probe of the old address time out after
 * 10 s each), so rounds do not overlap; it costs one Bonjour query and one
 * connection to the Mac, and stays well under the 5 min backoff cap.
 */
export const RERESOLVE_INTERVAL_MS = 30_000;

/** A request that reached no Mac: no route, no answer, or a pin mismatch. */
function failedToConnect(error: unknown): boolean {
	return error instanceof HandoverError && error.kind === "unreachable";
}

/** Whether the paired Mac answers at `endpoint`; any answer through the pin counts. */
function answers(endpoint: MacEndpoint): Promise<boolean> {
	return pinnedRequest(endpoint, "GET", paths.hello).then(
		() => true,
		() => false,
	);
}

const recordingFiles: RecordingFiles = {
	exists: (fileName) => queuedFile(fileName).exists,
	uri: (fileName) => queuedFile(fileName).uri,
	remove: (fileName) => queuedFile(fileName).delete(),
};

export function useUploadCoordinator(): UploadCoordinator {
	const {
		pairing,
		ready: pairingReady,
		currentToken,
		clearIfCurrent,
	} = usePairing();
	const { index, ready: queueReady, update } = useQueue();
	const discovery = useMacDiscovery(pairingReady && pairing !== null);
	const [resolved, setResolved] = useState<MacSession | null>(null);
	// A session belongs to the pairing whose token it carries: no tick that
	// starts after the pairing changes plans with the old token or the old
	// Mac's origin, not even before the new Mac resolves.
	const session = resolved?.token === pairing?.token ? resolved : null;
	const [progress, setProgress] = useState<Record<string, number>>({});
	const ticking = useRef(false);
	const rerun = useRef(false);
	const waitTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
	const indexRef = useRef(index);
	const sessionRef = useRef(session);
	const unmounted = useRef(false);
	const [foreground, setForeground] = useState(
		() => AppState.currentState !== "background",
	);
	// The session last chosen for the pairing, kept while the service is
	// away; set the moment a new address is chosen, before it renders.
	const adopted = useRef<MacSession | null>(null);
	const [resolveRound, setResolveRound] = useState(0);
	const resolving = useRef(false);
	// Asks for one more resolve; a resolve already running answers it.
	const resolveAgain = useCallback(() => {
		if (!resolving.current) setResolveRound((round) => round + 1);
	}, []);

	const executor = useMemo(() => {
		// A tick already running when a re-pairing starts still holds the old
		// session: its next request is refused, and a chunk whose task was
		// still being created, so the re-pairing's cancel missed it, is
		// cancelled once it exists. The same holds for a chunk to an address
		// the Mac left. A request that fails to connect resolves the Mac
		// again, so the retry after the backoff goes to its new address.
		const whileCurrent =
			<Args extends unknown[], Result>(
				request: (session: MacSession, ...args: Args) => Promise<Result>,
			) =>
			async (session: MacSession, ...args: Args): Promise<Result> => {
				if (session.token !== currentToken()) throw new Error("Retrying");
				try {
					return await request(session, ...args);
				} catch (error) {
					if (failedToConnect(error)) resolveAgain();
					throw error;
				}
			};
		return createUploadExecutor({
			client: {
				announce: whileCurrent(announce),
				status: whileCurrent(fetchStatus),
				complete: whileCurrent(complete),
				startChunkUpload: whileCurrent(
					async (session, recordingID, chunk, uri) => {
						await startChunkUpload(session, recordingID, chunk, uri);
						if (
							session.token !== currentToken() ||
							session.endpoint.origin !== adopted.current?.endpoint.origin
						) {
							// Logged, not thrown: a throw would stop tracking a task
							// that is still running. It stays tracked until its
							// result arrives.
							await stenoLink()
								.cancelUpload(taskIDs.chunk(recordingID, chunk.index))
								.catch((error) => console.warn("[sync] cancel failed", error));
						}
					},
				),
			},
			files: recordingFiles,
			deviceName: async () => (await deviceIdentity()).deviceName,
			update,
			onUnauthorized: clearIfCurrent,
			now: () => new Date(),
			random: Math.random,
		});
	}, [update, currentToken, clearIfCurrent, resolveAgain]);

	const serviceName = pairing
		? (findByMacID(discovery.services, pairing.mac.macID)?.name ?? null)
		: null;

	// Resolve the Mac once per appearance and pairing, and again for every
	// round `resolveAgain` asks for; forget it when the service goes. The
	// address in use is kept while it answers, so a Mac on two networks of
	// one LAN does not flip between them. Once it stops answering the new
	// address takes over and the chunks still out are cancelled: the
	// background session would retry them at the old address for days. A
	// cancelled chunk backs off and is sent again; the Mac keeps what it has.
	// biome-ignore lint/correctness/useExhaustiveDependencies: a new round is what resolves again.
	useEffect(() => {
		if (!pairing || !serviceName) {
			setResolved(null);
			return;
		}
		let cancelled = false;
		resolving.current = true;
		const { fingerprint } = pairing.mac;
		const token = pairing.token;
		const run = async () => {
			const mac = await stenoLink().resolve(serviceName);
			const found: MacSession = {
				endpoint: { origin: macOrigin(mac), fingerprint },
				token,
			};
			const current = adopted.current;
			const samePairing =
				current?.token === token &&
				current.endpoint.fingerprint === fingerprint;
			if (
				samePairing &&
				(current.endpoint.origin === found.endpoint.origin ||
					(await answers(current.endpoint)))
			) {
				if (!cancelled) setResolved(current);
				return;
			}
			if (cancelled) return;
			adopted.current = found;
			if (samePairing) {
				await cancelAllUploads().catch((error) =>
					console.warn("[sync] cancel failed", error),
				);
			}
			if (!cancelled) setResolved(found);
		};
		void run()
			.catch((error) => {
				if (!cancelled) console.warn("[sync] resolve failed", error);
			})
			.finally(() => {
				if (!cancelled) resolving.current = false;
			});
		return () => {
			cancelled = true;
			resolving.current = false;
		};
	}, [pairing, serviceName, resolveRound]);

	// While uploads are queued in the foreground, resolve again on a timer:
	// it finds a new address that sent no Bonjour event, and retries a
	// resolve that failed.
	const uploadsQueued = index.recordings.some(isPending);
	useEffect(() => {
		if (!pairing || !serviceName || !uploadsQueued || !foreground) return;
		const timer = setInterval(resolveAgain, RERESOLVE_INTERVAL_MS);
		return () => clearInterval(timer);
	}, [pairing, serviceName, uploadsQueued, foreground, resolveAgain]);

	// After unmount no timer is left and no new tick starts.
	useEffect(() => {
		unmounted.current = false;
		return () => {
			unmounted.current = true;
			if (waitTimer.current) clearTimeout(waitTimer.current);
			waitTimer.current = null;
		};
	}, []);

	const tick = useCallback(() => {
		if (!queueReady || unmounted.current) return;
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
			if (active && active.token !== currentToken()) {
				// The pairing is changing: plan nothing under the old one, and
				// look again shortly in case the re-pairing fails and it stays.
				waitTimer.current = setTimeout(() => tick(), REPAIRING_RECHECK_MS);
				return;
			}
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
	}, [queueReady, currentToken, executor]);

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
				void executor
					.uploadFinished(event)
					.catch((error) => console.warn("[sync] upload event failed", error))
					.finally(tick);
			}),
			// While the queue cannot be loaded the update rejects and the chunk
			// mark is dropped; the Mac's status restores it at the next announce.
			link.addListener("uploadFailed", (event) => {
				// No answer or a rejected pin, not the app's own cancel: the Mac
				// may have moved.
				if (event.retryable) resolveAgain();
				void executor
					.uploadFailed(event)
					.catch((error) => console.warn("[sync] upload event failed", error))
					.finally(tick);
			}),
		];
		return () => {
			for (const sub of subs) sub.remove();
		};
	}, [executor, tick, resolveAgain]);

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
			setForeground(state !== "background");
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
				canRetry(findRecording(current, recordingID))
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
