import { useEffect, useState } from "react";
import { send, useBridge, usePageReady } from "@/bridge/hooks";
import { ColumnResizer } from "@/components/ui";
import {
	COLUMNS,
	type ColumnName,
	columnMax,
	fitColumns,
	useColumnWidths,
} from "./column-widths";
import { MeetingDetail } from "./meeting-detail";
import { MeetingList } from "./meeting-list";
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
 * for the limits and where the widths are kept). Tells the host the page is ready once and reports the window size
 * after a resize settles. A deep link to a meeting is the host's to follow:
 * it selects the meeting and the page shows `list.selection`.
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

	const resizer = (name: ColumnName) => (
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
	);

	return (
		<div
			className="grid h-full min-h-0 overflow-hidden bg-background text-foreground"
			data-testid="main-window"
			style={{
				gridTemplateColumns: `${widths.sidebar}px ${widths.list}px minmax(0,1fr)`,
			}}
		>
			<div className="relative grid min-h-0 min-w-0">
				<Sidebar />
				{resizer("sidebar")}
			</div>
			<div className="relative grid min-h-0 min-w-0">
				<MeetingList />
				{resizer("list")}
			</div>
			<MeetingDetail
				initialMenuOpen={menuOpen}
				initialPickerOpen={pickerOpen}
			/>
		</div>
	);
}
