import {
	CheckCircle2Icon,
	CircleAlertIcon,
	ClockIcon,
	InboxIcon,
	PhoneIcon,
	SettingsIcon,
	SmartphoneIcon,
	UsersIcon,
} from "lucide-react";
import type { ReactNode } from "react";
import type {
	MeetingsListSnapshot,
	RecordingSnapshot,
} from "@/bridge/contract";
import { send, useBridge, useSnapshot } from "@/bridge/hooks";
import {
	Button,
	Card,
	MenuItem,
	MenuPopup,
	RecordMark,
	SectionLabel,
	SidebarRow,
	SplitButton,
} from "@/components/ui";
import { cn } from "@/lib/cn";
import { format } from "./format";
import { useElapsedSeconds } from "./use-now";

type Filter = MeetingsListSnapshot["filter"];

const FILTERS: readonly { id: Filter; label: string; icon: ReactNode }[] = [
	{ id: "all", label: "All", icon: <InboxIcon /> },
	{ id: "processing", label: "In progress", icon: <ClockIcon /> },
	{ id: "ready", label: "Ready", icon: <CheckCircle2Icon /> },
	{ id: "failed", label: "Failed", icon: <CircleAlertIcon /> },
];

/** The Record control: one primary button whose words follow the recorder. */
function RecordControl({
	recording,
}: {
	recording: RecordingSnapshot | undefined;
}) {
	const client = useBridge();
	const state = recording?.state ?? "idle";
	const elapsed = useElapsedSeconds(
		state === "recording" ? recording?.startedAt : undefined,
	);

	if (state === "recording") {
		return (
			<Button
				className="mb-3 h-9 w-full justify-start"
				data-testid="sidebar-stop"
				onClick={() => send(client, "recording.stop")}
				size="lg"
				variant="primary"
			>
				<RecordMark />
				Stop
				<span className="ml-auto font-mono font-normal text-[12.5px] tabular-nums">
					{format.duration(elapsed)}
				</span>
			</Button>
		);
	}
	if (state === "starting" || state === "stopping") {
		return (
			<Button
				className="mb-3 h-9 w-full justify-start"
				data-testid={state === "starting" ? "sidebar-record" : "sidebar-stop"}
				disabled
				size="lg"
				variant="primary"
			>
				<RecordMark />
				{state === "starting" ? "Starting…" : "Stopping…"}
			</Button>
		);
	}
	return (
		<SplitButton
			className="mb-3 w-full"
			data-testid="sidebar-record"
			menu={
				<MenuPopup align="end" sideOffset={4}>
					<MenuItem
						data-testid="record-call"
						icon={<PhoneIcon />}
						onClick={() => send(client, "recording.start", { mode: "call" })}
					>
						Record call
					</MenuItem>
					<MenuItem
						data-testid="sidebar-record-in-person"
						icon={<UsersIcon />}
						onClick={() =>
							send(client, "recording.start", { mode: "inPerson" })
						}
					>
						Record in person
					</MenuItem>
				</MenuPopup>
			}
			menuLabel="Other ways to record"
			menuTestId="sidebar-record-menu"
			onClick={() => send(client, "recording.start", { mode: "call" })}
			size="lg"
			variant="primary"
		>
			<RecordMark />
			Record
		</SplitButton>
	);
}

/**
 * The 236 pt column: the Record control, the filters with counts, the tags,
 * then the paired iPhone and Settings at the foot.
 */
export function Sidebar() {
	const client = useBridge();
	const app = useSnapshot("app");
	const recording = useSnapshot("recording");
	const list = useSnapshot("meetings.list");
	const phone = app?.phone;

	return (
		<aside className="surface-grain flex min-h-0 flex-col gap-0.5 border-border border-r bg-sidebar px-2 pt-[52px] pb-2">
			<RecordControl recording={recording} />
			<SectionLabel>Meetings</SectionLabel>
			{FILTERS.map((item) => (
				<SidebarRow
					active={list?.filter === item.id}
					count={list?.counts[item.id]}
					data-testid={`nav-${item.id}`}
					icon={item.icon}
					key={item.id}
					onClick={() =>
						send(client, "meetings.setFilter", { filter: item.id })
					}
				>
					{item.label}
				</SidebarRow>
			))}
			{list && list.tags.length > 0 ? (
				<>
					<SectionLabel>Tags</SectionLabel>
					{list.tags.map((tag) => {
						const active = list.tagFilter === tag.name;
						return (
							<SidebarRow
								active={active}
								data-testid={`tag-${tag.name}`}
								key={tag.name}
								onClick={() =>
									send(
										client,
										"meetings.setTagFilter",
										active ? {} : { tag: tag.name },
									)
								}
								variant="tag"
							>
								{tag.name}
							</SidebarRow>
						);
					})}
				</>
			) : null}
			<div className="mt-auto flex flex-col gap-1.5">
				{phone ? (
					<Card
						className="flex items-center gap-[9px]"
						data-testid="phone-card"
						padding="sm"
					>
						<SmartphoneIcon
							aria-hidden="true"
							className="size-4 shrink-0 stroke-[1.75] text-muted-foreground"
						/>
						<span className="min-w-0 flex-1 text-muted-foreground text-xs">
							<span className="block truncate font-medium text-[12.5px] text-foreground">
								{phone.name}
							</span>
							{phone.lastSyncAt
								? `Synced ${format.relative(phone.lastSyncAt)}`
								: "Not synced yet"}
						</span>
						<span
							aria-label={phone.isReachable ? "Connected" : "Not connected"}
							className={cn(
								"size-[7px] shrink-0 rounded-full",
								phone.isReachable
									? "bg-primary-2 shadow-[0_0_0_3px_var(--primary-soft)]"
									: "bg-faint",
							)}
							role="img"
						/>
					</Card>
				) : null}
				<SidebarRow
					data-testid="nav-settings"
					icon={<SettingsIcon />}
					onClick={() => send(client, "window.open", { window: "settings" })}
				>
					Settings
				</SidebarRow>
			</div>
		</aside>
	);
}
