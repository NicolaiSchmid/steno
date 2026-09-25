import { macOrigin } from "@modules/steno-link";
import { useNavigation } from "@react-navigation/native";
import {
	type BarcodeScanningResult,
	CameraView,
	useCameraPermissions,
} from "expo-camera";
import { useCallback, useEffect, useRef, useState } from "react";
import { Linking, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";

import { AppText } from "@/components/AppText";
import { PressableScale } from "@/components/PressableScale";
import { Spinner } from "@/components/Spinner";
import {
	locateMac,
	useMacDiscovery,
} from "@/features/discovery/use-mac-discovery";
import { useQueue } from "@/features/queue/QueueProvider";
import { resetForUpload, unpairPending } from "@/features/queue/queue-index";
import { cancelAllUploads } from "@/features/sync/recording-client";
import { errorMessage } from "@/lib/error-message";
import { DURATION_ENTRANCE, HIT_SLOP } from "@/lib/motion";
import { usePairing } from "./PairingProvider";
import { hello, pair, unpair } from "./pairing-client";
import { performPairing } from "./pairing-flow";
import { describePairingFailure, parsePairingPayload } from "./pairing-payload";
import { deviceIdentity } from "./pairing-store";

/**
 * Modal: scan the Mac's QR code, pair over the pinned channel, or unpair.
 * A second scan replaces the pairing. Runs in the foreground only, because a
 * backgrounded app in the undetermined local-network state is denied silently.
 */
type Phase =
	| { kind: "scanning" }
	| { kind: "pairing"; macName: string }
	| { kind: "paired"; macName: string }
	| { kind: "error"; message: string };

/** After a bad scan, ignore frames for a moment so the error is readable. */
const RESCAN_DELAY_MS = 6 * DURATION_ENTRANCE;
const CLOSE_DELAY_MS = 4 * DURATION_ENTRANCE;

export function PairingSheet() {
	const navigation = useNavigation();
	const [permission, requestPermission] = useCameraPermissions();
	const { pairing, replace, clear } = usePairing();
	const { update } = useQueue();
	const discovery = useMacDiscovery();
	const [phase, setPhase] = useState<Phase>({ kind: "scanning" });
	const busy = useRef(false);
	const scanBlockedUntil = useRef(0);

	useEffect(() => {
		if (permission && !permission.granted && permission.canAskAgain) {
			void requestPermission();
		}
	}, [permission, requestPermission]);

	useEffect(() => {
		if (phase.kind !== "paired") return;
		const timer = setTimeout(() => navigation.goBack(), CLOSE_DELAY_MS);
		return () => clearTimeout(timer);
	}, [phase, navigation]);

	const onScanned = useCallback(
		async ({ data }: BarcodeScanningResult) => {
			if (busy.current || Date.now() < scanBlockedUntil.current) return;
			const parsed = parsePairingPayload(data, new Date());
			if (!parsed.ok) {
				scanBlockedUntil.current = Date.now() + RESCAN_DELAY_MS;
				setPhase({
					kind: "error",
					message: describePairingFailure(parsed.reason),
				});
				return;
			}
			busy.current = true;
			setPhase({ kind: "pairing", macName: parsed.payload.macName });
			try {
				const outcome = await performPairing(parsed.payload, {
					locate: locateMac,
					hello,
					pair,
					device: deviceIdentity,
					now: () => new Date(),
				});
				// Chunks in flight for the old pairing would answer 401 under the
				// new one and wipe it; the retry backoff re-queues them.
				await cancelAllUploads().catch(() => {});
				await replace(outcome);
				// Anything the old Mac revoked is eligible for the new one.
				await update((index) =>
					index.recordings
						.filter((r) => r.state === "unpaired")
						.reduce((acc, r) => resetForUpload(acc, r.recordingID), index),
				);
				setPhase({ kind: "paired", macName: outcome.mac.macName });
			} catch (error) {
				scanBlockedUntil.current = Date.now() + RESCAN_DELAY_MS;
				setPhase({ kind: "error", message: errorMessage(error) });
			} finally {
				busy.current = false;
			}
		},
		[replace, update],
	);

	const onUnpair = useCallback(async () => {
		if (!pairing || busy.current) return;
		busy.current = true;
		try {
			try {
				const resolved = await locateMac(pairing.mac.macID, 3000);
				await unpair(
					{ origin: macOrigin(resolved), fingerprint: pairing.mac.fingerprint },
					pairing.token,
				);
			} catch {
				// The Mac is away; forgetting locally is what the user asked for.
			}
			await clear();
			await update(unpairPending);
			navigation.goBack();
		} finally {
			busy.current = false;
		}
	}, [pairing, clear, update, navigation]);

	const cameraDenied =
		permission !== null && !permission.granted && !permission.canAskAgain;
	const networkDenied = discovery.browser?.policyDenied === true;
	const scanning = phase.kind === "scanning" || phase.kind === "error";

	return (
		<SafeAreaView className="flex-1 bg-background" edges={["bottom"]}>
			<View className="flex-row items-center justify-between px-6 pt-5 pb-3">
				<AppText variant="title">Pair with your Mac</AppText>
				<PressableScale
					accessibilityLabel="Close"
					accessibilityRole="button"
					hitSlop={HIT_SLOP}
					onPress={() => navigation.goBack()}
				>
					<AppText variant="heading">Done</AppText>
				</PressableScale>
			</View>

			<View className="flex-1 gap-4 px-6">
				{pairing ? (
					<View className="gap-1 rounded-2xl border border-border bg-card p-4">
						<AppText variant="label">Paired</AppText>
						<AppText variant="heading">{pairing.mac.macName}</AppText>
						<AppText variant="muted">
							Scan a new code to pair with another Mac.
						</AppText>
					</View>
				) : (
					<AppText variant="muted">
						On the Mac, open Steno, choose Pair a phone, and hold the code in
						front of the camera.
					</AppText>
				)}

				<View className="aspect-square w-full overflow-hidden rounded-2xl border border-border bg-card">
					{permission?.granted ? (
						<CameraView
							active={scanning}
							barcodeScannerSettings={{ barcodeTypes: ["qr"] }}
							facing="back"
							onBarcodeScanned={scanning ? onScanned : undefined}
							style={{ flex: 1 }}
						/>
					) : (
						<View className="flex-1 items-center justify-center gap-3 p-6">
							<AppText className="text-center" variant="muted">
								{cameraDenied
									? "Camera access is off. Allow it in Settings to scan the code."
									: "Steno needs the camera to read the pairing code."}
							</AppText>
							<PressableScale
								accessibilityRole="button"
								onPress={() =>
									cameraDenied
										? void Linking.openSettings()
										: void requestPermission()
								}
							>
								<AppText variant="heading">
									{cameraDenied ? "Open Settings" : "Allow camera"}
								</AppText>
							</PressableScale>
						</View>
					)}
				</View>

				{networkDenied ? (
					<AppText variant="error">
						Local network access is off. Allow it for Steno in Settings, then
						try again.
					</AppText>
				) : null}

				<View className="min-h-12 flex-row items-center gap-3">
					{phase.kind === "pairing" ? (
						<>
							<Spinner accessibilityLabel="Pairing" />
							<AppText variant="body">Pairing with {phase.macName}…</AppText>
						</>
					) : null}
					{phase.kind === "paired" ? (
						<AppText variant="body">Paired with {phase.macName}.</AppText>
					) : null}
					{phase.kind === "error" ? (
						<AppText variant="error">{phase.message}</AppText>
					) : null}
				</View>
			</View>

			{pairing ? (
				<View className="px-6 pb-4">
					<PressableScale
						accessibilityLabel={`Unpair from ${pairing.mac.macName}`}
						accessibilityRole="button"
						onPress={() => void onUnpair()}
					>
						<View className="items-center rounded-2xl border border-border py-3">
							<AppText className="text-destructive" variant="heading">
								Unpair
							</AppText>
						</View>
					</PressableScale>
				</View>
			) : null}
		</SafeAreaView>
	);
}
