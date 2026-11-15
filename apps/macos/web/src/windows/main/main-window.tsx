import { type ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { send, useBridge, usePageReady, useSnapshot } from "@/bridge/hooks";
import {
	ColumnResizer,
	Drawer,
	DrawerCloseButton,
	DrawerPopup,
	ToastRegion,
} from "@/components/ui";
import {
	COLUMNS,
	type ColumnName,
	columnMax,
	fitColumns,
	isNarrow,
	useColumnWidths,
} from "./column-widths";
import { MeetingDetail } from "./meeting-detail";
import { MeetingList } from "./meeting-list";
import { NarrowLayoutContext } from "./narrow-layout";
import { Sidebar } from "./sidebar";

export const LAYOUT_DEBOUNCE_MS = 150;

export interface MainWindowProps {
	/** Opens the actions menu on mount (the screens). */
	menuOpen?: boolean;
	/** Opens the speaker picker on mount (the screens). */
	pickerOpen?: boolean;
}

const RESIZER_LABELS: Record<ColumnName, string> = {
	sidebar: "Resize sidebar",
	list: "Resize meeting list",
};

/**
 * The main window: sidebar (256 by default), meeting list (300) and the
 * reading column, with a divider on the trailing edge of the first two that
 * drags, takes arrow keys and resets on a double-click (`column-widths.ts`
 * for the limits and where the widths are kept). Tells the host the page is
 * ready once and reports the window size after a resize settles. A deep link
 * to a meeting is the host's to follow: it selects the meeting and the page
 * shows `list.selection`.
 *
 * In a window too narrow for all three (`isNarrow`, under 840 CSS px, as a
 * 1200 px window at 200 % zoom is), the reading column takes the window and
 * the sidebar and list open as a drawer from the header's Meetings button;
 * picking a meeting or its Close closes it, and the focus goes back to the
 * Meetings button. The recorder's status, Stop and its urgent notices stay
 * outside the drawer, in the reading column's recorder bar.
 */
export function MainWindow({
	menuOpen = false,
	pickerOpen = false,
}: MainWindowProps) {
	const client = useBridge();
	usePageReady(client);
	const { widths: stored, update, commit } = useColumnWidths();
	const [windowWidth, setWindowWidth] = useState(() => window.innerWidth);
	const widths = fitColumns(stored, windowWidth);
	const narrow = isNarrow(windowWidth);
	const [drawerOpen, setDrawerOpen] = useState(false);
	const toggleRef = useRef<HTMLButtonElement>(null);
	const selection = useSnapshot("meetings.list")?.selection;
	const layout = useMemo(
		() => ({
			narrow,
			openMeetings: () => setDrawerOpen(true),
			closeMeetings: () => setDrawerOpen(false),
			toggleRef,
		}),
		[narrow],
	);

	// A pick in the drawer, or a window wide enough again, closes it.
	// biome-ignore lint/correctness/useExhaustiveDependencies: the selection changing is the trigger.
	useEffect(() => {
		setDrawerOpen(false);
	}, [selection, narrow]);

	useEffect(() => {
		let timer: ReturnType<typeof setTimeout> | undefined;
		const report = () =>
			send(client, "page.layout", {
				window: "main",
				width: window.innerWidth,
				height: window.innerHeight,
			});
		const onResize = () => {
			setWindowWidth(window.innerWidth);
			clearTimeout(timer);
			timer = setTimeout(report, LAYOUT_DEBOUNCE_MS);
		};
		window.addEventListener("resize", onResize);
		return () => {
			clearTimeout(timer);
			window.removeEventListener("resize", onResize);
		};
	}, [client]);

	const column = (name: ColumnName, content: ReactNode) => (
		<div className="relative grid min-h-0 min-w-0">
			{content}
			<ColumnResizer
				aria-label={RESIZER_LABELS[name]}
				data-testid={`resize-${name}`}
				defaultValue={COLUMNS[name].default}
				max={columnMax(name, widths, windowWidth)}
				min={COLUMNS[name].min}
				onChange={(width) => update(name, width)}
				onCommit={(width) => commit(name, width)}
				value={widths[name]}
			/>
		</div>
	);

	const detail = (
		<MeetingDetail initialMenuOpen={menuOpen} initialPickerOpen={pickerOpen} />
	);

	if (narrow) {
		return (
			<NarrowLayoutContext.Provider value={layout}>
				<div
					className="grid h-full min-h-0 grid-cols-[minmax(0,1fr)] overflow-hidden bg-background text-foreground"
					data-narrow=""
					data-testid="main-window"
				>
					{detail}
				</div>
				<Drawer onOpenChange={setDrawerOpen} open={drawerOpen}>
					<DrawerPopup
						aria-label="Meetings"
						data-testid="meetings-drawer"
						finalFocus={() => toggleRef.current}
					>
						<div
							className="grid h-full min-h-0"
							style={{
								gridTemplateColumns: `${COLUMNS.sidebar.min}px ${COLUMNS.list.min}px`,
							}}
						>
							<Sidebar />
							<MeetingList />
						</div>
						<DrawerCloseButton data-testid="close-meetings" />
					</DrawerPopup>
				</Drawer>
				<ToastRegion />
			</NarrowLayoutContext.Provider>
		);
	}

	return (
		<NarrowLayoutContext.Provider value={layout}>
			<div
				className="grid h-full min-h-0 overflow-hidden bg-background text-foreground"
				data-testid="main-window"
				style={{
					gridTemplateColumns: `${widths.sidebar}px ${widths.list}px minmax(0,1fr)`,
				}}
			>
				{column("sidebar", <Sidebar />)}
				{column("list", <MeetingList />)}
				{detail}
			</div>
			<ToastRegion />
		</NarrowLayoutContext.Provider>
	);
}
