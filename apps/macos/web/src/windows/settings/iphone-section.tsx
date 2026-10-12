import { CircleAlertIcon, SmartphoneIcon } from "lucide-react";
import type { PhoneSettingsSnapshot } from "@/bridge/contract";
import { send, useBridge, useSnapshot } from "@/bridge/hooks";
import {
	Button,
	Callout,
	Card,
	Disclosure,
	FormCard,
	FormRow,
	FormValue,
	ProgressBar,
	QRCode,
} from "@/components/ui";
import { usePlatform } from "@/lib/platform";
import { useNow } from "@/lib/use-now";
import { SectionPage } from "./section-page";
import {
	deviceName,
	pairedText,
	pairingExpiryText,
	transferProgress,
} from "./settings-format";

/** The QR code for the iPhone app, with its expiry and a Cancel. */
function PairingCard({
	pairing,
}: {
	pairing: NonNullable<PhoneSettingsSnapshot["pairing"]>;
}) {
	const client = useBridge();
	// The expiry sentence changes by the minute; a 15 s tick is plenty.
	const now = useNow(true, 15_000);
	const expiry = pairingExpiryText(pairing.expiresAt, new Date(now));
	return (
		<Card
			className="flex flex-col items-center gap-3"
			data-testid="pairing-card"
			padding="lg"
		>
			<QRCode
				alt="Pairing code for the Steno iPhone app"
				pngBase64={pairing.qrPNGBase64}
				size={180}
			/>
			<p className="m-0 max-w-[320px] text-center text-muted-foreground text-sm">
				Scan this code with the Steno app on your iPhone.{" "}
				<span data-testid="pairing-expiry">
					{expiry ?? "The code has expired."}
				</span>
			</p>
			<Button
				data-testid="cancel-pairing"
				onClick={() => send(client, "settings.iphone.cancelPairing")}
				size="sm"
				variant="outline"
			>
				Cancel
			</Button>
		</Card>
	);
}

/**
 * Phone handover waits: the host's sentence on what happened, Try again,
 * and Pair again with what it costs. The host asks before Pair again goes
 * ahead; both are off while one of them runs.
 */
function WaitingCard({
	waiting,
}: {
	waiting: NonNullable<PhoneSettingsSnapshot["waiting"]>;
}) {
	const client = useBridge();
	const { words } = usePlatform();
	return (
		<div className="flex flex-col gap-3" data-testid="phone-waiting">
			<Callout
				description={waiting.message}
				icon={<CircleAlertIcon aria-hidden="true" />}
				title="Your phones can't send recordings right now."
				variant="warning"
			/>
			<FormCard title="Phone pairing">
				<FormRow
					control={
						<Button
							data-testid="try-again"
							disabled={waiting.busy}
							onClick={() => send(client, "settings.iphone.tryAgain")}
							size="sm"
							variant="primary"
						>
							{waiting.busy ? "Trying…" : "Try again"}
						</Button>
					}
					description={`Steno reads this ${words.computer}'s phone pairing again. Your phones keep their pairing.`}
					label="Try again"
				/>
				<FormRow
					control={
						<Button
							data-testid="pair-again"
							disabled={waiting.busy}
							onClick={() => send(client, "settings.iphone.pairAgain")}
							size="sm"
							variant="outline"
						>
							Pair again…
						</Button>
					}
					data-testid="pair-again-row"
					description="Every phone must pair again. Steno removes them from the list, and each one scans a new code."
					label="Start over"
				/>
			</FormCard>
		</div>
	);
}

/**
 * iPhone: the paired phones, the pairing code, transfers arriving, and the
 * listener's failure when it has one. While phone handover waits, Try again
 * and Pair again take the place of a new pairing, and the paired phones are
 * listed without Remove.
 */
export function PhoneSection() {
	const client = useBridge();
	const { words } = usePlatform();
	const phone = useSnapshot("settings.iphone");
	if (!phone) {
		return <SectionPage id="iphone" />;
	}
	const waiting = phone.waiting;
	if (phone.listener.state === "unavailable") {
		return (
			<SectionPage
				error={phone.error}
				errorDetails={phone.errorDetails}
				id="iphone"
			>
				<Callout
					data-testid="phone-unavailable"
					description="Relaunch Steno to try again."
					icon={<CircleAlertIcon aria-hidden="true" />}
					title="Pairing is unavailable right now."
					variant="warning"
				/>
			</SectionPage>
		);
	}

	return (
		<SectionPage
			error={phone.error}
			errorDetails={phone.errorDetails}
			id="iphone"
		>
			{waiting ? <WaitingCard waiting={waiting} /> : null}
			{phone.pairing && !waiting ? (
				<PairingCard pairing={phone.pairing} />
			) : null}

			<FormCard
				footer={`The first pairing asks for local network access. Recordings travel over your Wi-Fi only, encrypted to this ${words.computer}.`}
				title="Paired phones"
			>
				{phone.devices.length === 0 ? (
					<FormRow
						data-testid="no-phones"
						description={`Pair one to record away from the ${words.computer}.`}
						label="No iPhone paired yet."
					/>
				) : null}
				{phone.devices.map((device) => (
					<FormRow
						control={
							waiting ? undefined : (
								<Button
									aria-label={`Remove ${device.name}`}
									data-testid={`revoke-${device.id}`}
									onClick={() =>
										send(client, "settings.iphone.revoke", {
											deviceID: device.id,
										})
									}
									size="sm"
									variant="ghost"
								>
									Remove
								</Button>
							)
						}
						data-testid={`device-${device.id}`}
						description={pairedText(device)}
						icon={<SmartphoneIcon aria-hidden="true" />}
						key={device.id}
						label={device.name}
					/>
				))}
				{waiting ? null : (
					<FormRow
						control={
							<Button
								data-testid="begin-pairing"
								disabled={phone.pairing !== undefined}
								onClick={() => send(client, "settings.iphone.beginPairing")}
								size="sm"
								variant="primary"
							>
								Pair an iPhone…
							</Button>
						}
						label="Add a phone"
					/>
				)}
			</FormCard>

			{phone.receipts.length > 0 ? (
				<FormCard title="Receiving">
					{phone.receipts.map((receipt) => {
						const progress = transferProgress(receipt);
						return (
							<FormRow
								control={
									<FormValue variant="mono">
										{Math.round(progress * 100)}%
									</FormValue>
								}
								data-testid={`receipt-${receipt.recordingID}`}
								key={receipt.recordingID}
								label={`Receiving from ${deviceName(phone, receipt.deviceID)}…`}
							>
								<ProgressBar
									aria-label="Transfer progress"
									value={Math.round(progress * 100)}
								/>
							</FormRow>
						);
					})}
				</FormCard>
			) : null}

			{phone.listener.state === "failed" && !waiting ? (
				<div className="flex flex-col gap-1.5" data-testid="listener-failed">
					<Callout
						icon={<CircleAlertIcon aria-hidden="true" />}
						title="Steno cannot receive recordings right now."
						variant="warning"
					/>
					{phone.listener.failure ? (
						<Disclosure>{phone.listener.failure}</Disclosure>
					) : null}
				</div>
			) : null}
		</SectionPage>
	);
}
