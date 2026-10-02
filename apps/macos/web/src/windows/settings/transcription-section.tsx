import { CircleAlertIcon } from "lucide-react";
import type { TranscriptionSettingsSnapshot } from "@/bridge/contract";
import { send, useBridge, useSnapshot } from "@/bridge/hooks";
import {
	Button,
	Callout,
	Disclosure,
	FormCard,
	FormRow,
	ProgressBar,
	Select,
} from "@/components/ui";
import { SectionPage } from "./section-page";

type Asset = TranscriptionSettingsSnapshot["assets"][number];

/** One component on this Mac: its state, and the action that fits it. */
function AssetRow({ asset }: { asset: Asset }) {
	const client = useBridge();
	const download = () =>
		send(client, "settings.transcription.download", { assetID: asset.id });
	let control: React.ReactNode;
	switch (asset.state) {
		case "absent":
			control = (
				<Button
					data-testid={`asset-${asset.id}-download`}
					onClick={download}
					size="sm"
					variant="outline"
				>
					Download
				</Button>
			);
			break;
		case "downloading":
			control = (
				<span
					className="font-mono text-faint text-xs tabular-nums"
					data-testid={`asset-${asset.id}-progress`}
				>
					{asset.downloadFraction !== undefined && asset.downloadFraction > 0
						? `${Math.round(asset.downloadFraction * 100)}%`
						: "…"}
				</span>
			);
			break;
		case "installed":
			control = (
				<Button
					data-testid={`asset-${asset.id}-remove`}
					onClick={() =>
						send(client, "settings.transcription.remove", { assetID: asset.id })
					}
					size="sm"
					variant="ghost"
				>
					Remove
				</Button>
			);
			break;
		case "failed":
			control = (
				<Button
					data-testid={`asset-${asset.id}-retry`}
					onClick={download}
					size="sm"
					variant="outline"
				>
					Retry
				</Button>
			);
			break;
	}
	return (
		<FormRow
			control={control}
			data-testid={`asset-${asset.id}`}
			description={asset.detail}
			label={asset.name}
		>
			{asset.state === "downloading" ? (
				<ProgressBar
					aria-label={`Downloading ${asset.name}`}
					value={
						asset.downloadFraction !== undefined && asset.downloadFraction > 0
							? Math.round(asset.downloadFraction * 100)
							: null
					}
				/>
			) : null}
			{asset.state === "failed" ? (
				<div className="flex flex-col gap-1.5">
					<Callout
						icon={<CircleAlertIcon aria-hidden="true" />}
						size="sm"
						title="The download did not finish."
						variant="warning"
					/>
					{asset.failure ? (
						<Disclosure data-testid={`asset-${asset.id}-failure`}>
							{asset.failure}
						</Disclosure>
					) : null}
				</div>
			) : null}
		</FormRow>
	);
}

/**
 * Transcription: the engine (when there is a choice) and the components it
 * needs on this Mac, with download progress.
 */
export function TranscriptionSection() {
	const client = useBridge();
	const transcription = useSnapshot("settings.transcription");
	if (!transcription) {
		return <SectionPage id="transcription" />;
	}
	return (
		<SectionPage
			error={transcription.error}
			errorDetails={transcription.errorDetails}
			id="transcription"
		>
			{transcription.showsEnginePicker ? (
				<FormCard
					footer="Both run on this Mac. Parakeet is quicker; Whisper understands more languages."
					title="Language model"
				>
					<FormRow
						control={
							<Select
								aria-label="Model"
								className="w-56"
								data-testid="engine"
								onValueChange={(value) => {
									if (value) {
										send(client, "settings.transcription.setEngine", { value });
									}
								}}
								options={transcription.engines.map((engine) => ({
									value: engine.id,
									label: engine.name,
								}))}
								size="sm"
								value={transcription.engineID}
							/>
						}
						label="Model"
					/>
				</FormCard>
			) : null}
			<FormCard
				footer={
					transcription.allInstalled
						? "Everything needed for transcription is installed."
						: "Downloads happen once and are kept for later meetings."
				}
				title="On this Mac"
			>
				{transcription.assets.map((asset) => (
					<AssetRow asset={asset} key={asset.id} />
				))}
			</FormCard>
		</SectionPage>
	);
}
