import {
	MicIcon,
	SettingsIcon,
	SmartphoneIcon,
	SparklesIcon,
	TextQuoteIcon,
	UploadIcon,
} from "lucide-react";
import { type ReactNode, useEffect, useState } from "react";
import { useBridge, usePageReady } from "@/bridge/hooks";
import { ContentColumn, SidebarColumn, SidebarRow } from "@/components/ui";
import { ExportSection } from "./export-section";
import { GeneralSection } from "./general-section";
import { PhoneSection } from "./iphone-section";
import { RecordingSection } from "./recording-section";
import { SECTIONS, type SectionId, sectionInfo } from "./sections";
import { SummariesSection } from "./summaries-section";
import { TranscriptionSection } from "./transcription-section";

const ICONS: Record<SectionId, ReactNode> = {
	general: <SettingsIcon />,
	recording: <MicIcon />,
	transcription: <TextQuoteIcon />,
	summaries: <SparklesIcon />,
	export: <UploadIcon />,
	iphone: <SmartphoneIcon />,
};

export interface SettingsWindowProps {
	/** The section from the route; a change selects it without a reload. */
	section?: SectionId | undefined;
}

/**
 * The Settings window: a 256 px sidebar of the six sections as single-line
 * rows, and the selected section under a "Settings / Section" breadcrumb in
 * the header row, its form cards centred at the settings width. Tells the
 * host the page is ready once. A deep link arrives as the `app` snapshot's
 * `requestedSettingsSection`, consumed by the host with that publish, and
 * wins over the route.
 */
export function SettingsWindow({ section: routeSection }: SettingsWindowProps) {
	const client = useBridge();
	const [section, setSection] = useState<SectionId>(routeSection ?? "general");
	usePageReady(client);

	useEffect(() => {
		if (routeSection) {
			setSection(routeSection);
		}
	}, [routeSection]);

	// A subscription of its own, not an effect over the `app` state: the host
	// publishes the snapshot that carries a deep link and, having cleared the
	// request, the clean one right after. React batches both into one render,
	// so an effect would only ever see the clean snapshot; the subscription
	// sees each publish, and the same section twice is two publishes.
	useEffect(
		() =>
			client.subscribe("app", (snapshot) => {
				const requested = snapshot.requestedSettingsSection;
				if (requested) setSection(requested);
			}),
		[client],
	);

	let page: ReactNode;
	switch (section) {
		case "general":
			page = <GeneralSection />;
			break;
		case "recording":
			page = <RecordingSection />;
			break;
		case "transcription":
			page = <TranscriptionSection />;
			break;
		case "summaries":
			page = <SummariesSection />;
			break;
		case "export":
			page = <ExportSection />;
			break;
		case "iphone":
			page = <PhoneSection />;
			break;
	}

	return (
		<div
			className="grid h-full min-h-0 grid-cols-[256px_minmax(0,1fr)] overflow-hidden bg-background text-foreground"
			data-testid="settings-window"
		>
			<SidebarColumn aria-label="Settings sections" as="nav">
				{SECTIONS.map((info) => (
					<SidebarRow
						active={info.id === section}
						data-testid={`settings-${info.id}`}
						icon={ICONS[info.id]}
						key={info.id}
						onClick={() => setSection(info.id)}
					>
						{info.title}
					</SidebarRow>
				))}
			</SidebarColumn>
			<div className="flex min-h-0 flex-col">
				<ContentColumn
					crumbs={["Settings", sectionInfo(section).title]}
					width="settings"
				>
					{page}
				</ContentColumn>
			</div>
		</div>
	);
}
