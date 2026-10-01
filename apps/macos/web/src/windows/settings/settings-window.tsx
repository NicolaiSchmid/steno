import {
	MicIcon,
	SettingsIcon,
	SmartphoneIcon,
	SparklesIcon,
	TextQuoteIcon,
	UploadIcon,
} from "lucide-react";
import { type ReactNode, useEffect, useRef, useState } from "react";
import { send, useBridge, usePageReady, useSnapshot } from "@/bridge/hooks";
import { ScrollArea, SidebarRow } from "@/components/ui";
import { ExportSection } from "./export-section";
import { GeneralSection } from "./general-section";
import { PhoneSection } from "./iphone-section";
import { RecordingSection } from "./recording-section";
import { SECTIONS, type SectionId, type SectionInfo } from "./sections";
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
 * The Settings window: a 200 pt sidebar of the six sections, each with the
 * one-line status its snapshot carries, and the selected section's form
 * cards. Tells the host the page is ready once. A deep link arrives as the
 * `app` snapshot's `requestedSettingsSection`, consumed by the host with
 * that publish, and wins over the route.
 */
export function SettingsWindow({ section: routeSection }: SettingsWindowProps) {
	const client = useBridge();
	const app = useSnapshot("app");
	const [section, setSection] = useState<SectionId>(routeSection ?? "general");
	usePageReady(client);

	useEffect(() => {
		if (routeSection) {
			setSection(routeSection);
		}
	}, [routeSection]);

	// Keyed on the snapshot, not the section string: the host publishes
	// `app` once per deep link and clears the request with that publish, so
	// the same section twice arrives as two snapshots and each is shown.
	useEffect(() => {
		const requested = app?.requestedSettingsSection;
		if (requested) setSection(requested);
	}, [app]);

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
			className="grid h-full min-h-0 grid-cols-[200px_minmax(0,1fr)] overflow-hidden bg-background text-foreground"
			data-testid="settings-window"
		>
			<nav
				aria-label="Settings sections"
				className="surface-grain flex min-h-0 flex-col gap-0.5 border-border border-r bg-sidebar px-2 pt-[52px] pb-2"
			>
				{SECTIONS.map((info) => (
					<SectionRow
						active={info.id === section}
						info={info}
						key={info.id}
						onSelect={() => setSection(info.id)}
					/>
				))}
			</nav>
			<ScrollArea className="min-h-0">
				<div className="mx-auto max-w-[640px]">{page}</div>
			</ScrollArea>
		</div>
	);
}

/** One sidebar row: the section's title over the subtitle its own snapshot carries. */
function SectionRow({
	info,
	active,
	onSelect,
}: {
	info: SectionInfo;
	active: boolean;
	onSelect: () => void;
}) {
	const snapshot = useSnapshot(info.topic);
	return (
		<SidebarRow
			active={active}
			data-testid={`settings-${info.id}`}
			icon={ICONS[info.id]}
			onClick={onSelect}
			subtitle={snapshot?.subtitle ?? "…"}
		>
			{info.title}
		</SidebarRow>
	);
}
