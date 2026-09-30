import { useEffect, useRef } from "react";
import { send, useBridge, useSnapshot } from "@/bridge/hooks";
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

/**
 * The main window: sidebar (236), meeting list (320) and the reading
 * column. Tells the host the page is ready once, reports the window size
 * after a resize settles, and follows a deep link to a meeting.
 */
export function MainWindow({
	menuOpen = false,
	pickerOpen = false,
}: MainWindowProps) {
	const client = useBridge();
	const app = useSnapshot("app");
	const list = useSnapshot("meetings.list");
	const readySent = useRef(false);
	const followed = useRef<string | undefined>(undefined);

	useEffect(() => {
		if (readySent.current) {
			return;
		}
		readySent.current = true;
		send(client, "page.ready");
	}, [client]);

	useEffect(() => {
		let timer: ReturnType<typeof setTimeout> | undefined;
		const report = () =>
			send(client, "page.layout", {
				window: "main",
				width: window.innerWidth,
				height: window.innerHeight,
			});
		const onResize = () => {
			clearTimeout(timer);
			timer = setTimeout(report, LAYOUT_DEBOUNCE_MS);
		};
		window.addEventListener("resize", onResize);
		return () => {
			clearTimeout(timer);
			window.removeEventListener("resize", onResize);
		};
	}, [client]);

	const requested = app?.requestedMeetingID;
	const selection = list?.selection;
	useEffect(() => {
		if (!requested || followed.current === requested) {
			return;
		}
		followed.current = requested;
		if (requested !== selection) {
			send(client, "meetings.select", { meetingID: requested });
		}
	}, [client, requested, selection]);

	return (
		<div
			className="grid h-full min-h-0 grid-cols-[236px_320px_minmax(0,1fr)] overflow-hidden bg-background text-foreground"
			data-testid="main-window"
		>
			<Sidebar />
			<MeetingList />
			<MeetingDetail
				initialMenuOpen={menuOpen}
				initialPickerOpen={pickerOpen}
			/>
		</div>
	);
}
