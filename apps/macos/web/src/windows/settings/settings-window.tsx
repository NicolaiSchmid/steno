import {
	MicIcon,
	SettingsIcon,
	SmartphoneIcon,
	SparklesIcon,
	TextQuoteIcon,
	UploadIcon,
} from "lucide-react";
import { type ReactNode, useEffect, useRef, useState } from "react";
import { send, useBridge, useSnapshot } from "@/bridge/hooks";
import { ScrollArea, SidebarRow } from "@/components/ui";
import { ExportSection } from "./export-section";
import { GeneralSection } from "./general-section";
import { PhoneSection } from "./iphone-section";
import { RecordingSection } from "./recording-section";
import { SECTIONS, type SectionId } from "./sections";
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
 * cards. Tells the host the page is ready once, and which section is shown
 * whenever that changes, so the host refreshes the subtitles and clears a
 * deep link once it has landed. A deep link arrives as the `app` snapshot's
 * `requestedSettingsSection` and wins over the route.
 */
export function SettingsWindow({ section: routeSection }: SettingsWindowProps) {
	const client = useBridge();
	const app = useSnapshot("app");
	const general = useSnapshot("settings.general");
	const recording = useSnapshot("settings.recording");
	const transcription = useSnapshot("settings.transcription");
	const summaries = useSnapshot("settings.summaries");
	const exportSettings = useSnapshot("settings.export");
	const phone = useSnapshot("settings.iphone");
	const [section, setSection] = useState<SectionId>(routeSection ?? "general");
	const readySent = useRef(false);

	useEffect(() => {
		if (readySent.current) {
			return;
		}
		readySent.current = true;
		send(client, "page.ready");
	}, [client]);

	useEffect(() => {
		if (routeSection) {
			setSection(routeSection);
		}
	}, [routeSection]);

	const requested = app?.requestedSettingsSection;
	useEffect(() => {
		if (requested) {
			setSection(requested);
		}
	}, [requested]);

	useEffect(() => {
		send(client, "settings.showSection", { section });
	}, [client, section]);

	const subtitles: Record<SectionId, string | undefined> = {
		general: general?.subtitle,
		recording: recording?.subtitle,
		transcription: transcription?.subtitle,
		summaries: summaries?.subtitle,
		export: exportSettings?.subtitle,
		iphone: phone?.subtitle,
	};

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
					<SidebarRow
						active={info.id === section}
						data-testid={`settings-${info.id}`}
						icon={ICONS[info.id]}
						key={info.id}
						onClick={() => setSection(info.id)}
						subtitle={subtitles[info.id] ?? "…"}
					>
						{info.title}
					</SidebarRow>
				))}
			</nav>
			<ScrollArea className="min-h-0">
				<div className="mx-auto max-w-[640px]">{page}</div>
			</ScrollArea>
		</div>
	);
}
