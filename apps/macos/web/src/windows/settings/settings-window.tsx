import {
	MicIcon,
	SettingsIcon,
	SmartphoneIcon,
	SparklesIcon,
	TextQuoteIcon,
	UploadIcon,
} from "lucide-react";
import { type ReactNode, useEffect, useRef, useState } from "react";
import { useBridge, usePageReady } from "@/bridge/hooks";
import {
	ContentColumn,
	Drawer,
	DrawerCloseButton,
	DrawerPopup,
	DrawerToggle,
	SidebarColumn,
	SidebarRow,
	ToastRegion,
} from "@/components/ui";
import { cn } from "@/lib/cn";
import { usePlatform } from "@/lib/platform";
import { useWindowWidth } from "@/lib/use-window-width";
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

/**
 * Below this window width the 256 px sections column would leave the form
 * too little room (a 960 px window at 200 % zoom is 480 CSS px): the
 * sections move into a drawer behind the header's leading button and the
 * form takes the window, as the main window's narrow layout does.
 */
export const SETTINGS_NARROW_BELOW = 840;

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
 *
 * In a narrow window (`SETTINGS_NARROW_BELOW`) the sections open as a
 * drawer from the header's Sections button; a pick closes it and the focus
 * goes back to the button.
 */
export function SettingsWindow({ section: routeSection }: SettingsWindowProps) {
	const client = useBridge();
	const platform = usePlatform();
	const [section, setSection] = useState<SectionId>(routeSection ?? "general");
	const windowWidth = useWindowWidth();
	const narrow = windowWidth > 0 && windowWidth < SETTINGS_NARROW_BELOW;
	const [drawerOpen, setDrawerOpen] = useState(false);
	const toggleRef = useRef<HTMLButtonElement>(null);
	usePageReady(client);

	// A window wide enough again closes the drawer; so does a pick (below).
	// biome-ignore lint/correctness/useExhaustiveDependencies: the width changing is the trigger.
	useEffect(() => {
		setDrawerOpen(false);
	}, [narrow]);

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

	const sections = (
		<SidebarColumn
			aria-label="Settings sections"
			as="nav"
			className={narrow ? "w-64" : undefined}
			titleBarInset={platform.titleBarInset}
		>
			{SECTIONS.map((info) => (
				<SidebarRow
					active={info.id === section}
					data-testid={`settings-${info.id}`}
					icon={ICONS[info.id]}
					key={info.id}
					onClick={() => {
						setSection(info.id);
						setDrawerOpen(false);
					}}
				>
					{info.title}
				</SidebarRow>
			))}
		</SidebarColumn>
	);

	return (
		<div
			className={cn(
				"grid h-full min-h-0 overflow-hidden bg-background text-foreground",
				narrow
					? "grid-cols-[minmax(0,1fr)]"
					: "grid-cols-[256px_minmax(0,1fr)]",
			)}
			data-narrow={narrow ? "" : undefined}
			data-testid="settings-window"
		>
			{narrow ? null : sections}
			<div className="flex min-h-0 flex-col">
				<ContentColumn
					crumbs={["Settings"]}
					leading={
						narrow ? (
							<DrawerToggle
								aria-label="Show sections"
								data-testid="show-sections"
								onClick={() => setDrawerOpen(true)}
								ref={toggleRef}
								titleBarInset={platform.titleBarInset}
							/>
						) : null
					}
					width="settings"
				>
					{page}
				</ContentColumn>
			</div>
			{narrow ? (
				<Drawer onOpenChange={setDrawerOpen} open={drawerOpen}>
					<DrawerPopup
						aria-label="Settings sections"
						data-testid="sections-drawer"
						finalFocus={() => toggleRef.current}
					>
						{sections}
						<DrawerCloseButton data-testid="close-sections" />
					</DrawerPopup>
				</Drawer>
			) : null}
			<ToastRegion />
		</div>
	);
}
