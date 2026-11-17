import {
	CheckCircle2Icon,
	CircleAlertIcon,
	ClockIcon,
	InboxIcon,
	MicOffIcon,
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
	Callout,
	Card,
	MenuItem,
	MenuPopup,
	RecordMark,
	SectionLabel,
	SidebarColumn,
	SidebarRow,
	SplitButton,
} from "@/components/ui";
import { cn } from "@/lib/cn";
import { type PlatformWords, usePlatform } from "@/lib/platform";
import { useElapsedSeconds } from "@/lib/use-now";
import { format } from "./format";
import { RecorderNotices } from "./recorder-notices";

type Filter = MeetingsListSnapshot["filter"];
type PermissionKind = RecordingSnapshot["deniedPermissions"][number];

const FILTERS: readonly { id: Filter; label: string; icon: ReactNode }[] = [
	{ id: "all", label: "All", icon: <InboxIcon /> },
	{ id: "processing", label: "In progress", icon: <ClockIcon /> },
	{ id: "ready", label: "Ready", icon: <CheckCircle2Icon /> },
	{ id: "failed", label: "Failed", icon: <CircleAlertIcon /> },
];

/**
 * What a denied permission stops Steno from doing, and what allows it, in
 * the platform's name for the place its privacy switches live.
 */
function deniedCopy(
	kind: PermissionKind,
	{ systemSettings }: PlatformWords,
): { title: string; description: string } {
	switch (kind) {
		case "microphone":
			return {
				title: "Steno can't use the microphone.",
				description: `Allow it in ${systemSettings} to record.`,
			};
		case "systemAudio":
			return {
				title: "Steno can't hear the other side of calls.",
				description: `Allow system audio recording in ${systemSettings}.`,
			};
		case "calendar":
			return {
				title: "Steno can't see your calendar.",
				description: `Allow calendar access in ${systemSettings}.`,
			};
		case "localNetwork":
			return {
				title: "Steno can't reach your iPhone.",
				description: `Allow local network access in ${systemSettings}.`,
			};
	}
}

/** The Record control: one primary button whose words follow the recorder. */
function RecordButton({
	recording,
}: {
	recording: RecordingSnapshot | undefined;
}) {
	const client = useBridge();
	const state = recording?.state ?? "idle";
	const elapsed = useElapsedSeconds(
		state === "recording" ? recording?.startedAt : undefined,
	);

	if (state === "recording" || state === "starting" || state === "stopping") {
		const busy = state !== "recording";
		return (
			<Button
				className="w-full justify-start"
				data-testid={state === "starting" ? "sidebar-record" : "sidebar-stop"}
				disabled={busy}
				onClick={busy ? undefined : () => send(client, "recording.stop")}
				variant="primary"
			>
				<RecordMark onPrimary pulse={state === "recording"} />
				{state === "recording"
					? "Stop"
					: state === "starting"
						? "Starting…"
						: "Stopping…"}
				{state === "recording" ? (
					<span className="ml-auto font-mono font-normal text-meta tabular-nums">
						{format.duration(elapsed)}
					</span>
				) : null}
			</Button>
		);
	}
	return (
		<SplitButton
			className="w-full"
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
			variant="primary"
		>
			<RecordMark />
			Record
		</SplitButton>
	);
}

/** A permission the recorder needs and does not have. */
function DeniedPermission({ kind }: { kind: PermissionKind }) {
	const client = useBridge();
	const { words } = usePlatform();
	const copy = deniedCopy(kind, words);
	return (
		<Callout
			actions={
				words.opensSystemSettings ? (
					<Button
						data-testid={`fix-${kind}`}
						onClick={() => send(client, "system.openSystemSettings", { kind })}
						size="sm"
						variant="outline"
					>
						Fix in {words.systemSettings}
					</Button>
				) : undefined
			}
			data-testid={`denied-${kind}`}
			description={copy.description}
			icon={<MicOffIcon aria-hidden="true" />}
			size="sm"
			title={copy.title}
			variant="warning"
		/>
	);
}

/**
 * The Record control and what the recorder has to say beneath it: the
 * auto-stop countdown, a warning or error (`RecorderNotices`), and any
 * permission it is missing.
 */
function RecordControl({
	recording,
}: {
	recording: RecordingSnapshot | undefined;
}) {
	return (
		<>
			<RecordButton recording={recording} />
			<RecorderNotices recording={recording} size="sm" />
			{recording?.deniedPermissions.map((kind) => (
				<DeniedPermission key={kind} kind={kind} />
			))}
		</>
	);
}

/**
 * The 256 px column: a header-high spacer under the traffic lights on the
 * Mac, the Record control, the filters with counts (no heading: the list
 * beside them is titled Meetings), the tags, then the paired iPhone and
 * Settings at the foot.
 */
export function Sidebar() {
	const client = useBridge();
	const platform = usePlatform();
	const app = useSnapshot("app");
	const recording = useSnapshot("recording");
	const list = useSnapshot("meetings.list");
	const phone = app?.phone;

	return (
		<SidebarColumn
			footer={
				<>
					{phone ? (
						<Card
							className="flex items-center gap-2"
							data-testid="phone-card"
							padding="sm"
							variant="sidebar"
						>
							<SmartphoneIcon
								aria-hidden="true"
								className="size-4 shrink-0 text-sidebar-icon"
							/>
							<span className="min-w-0 flex-1 text-meta text-sidebar-muted-foreground">
								<span className="block truncate text-foreground text-label">
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
										? "bg-success shadow-[0_0_0_3px_var(--success-surface)]"
										: "bg-outline",
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
				</>
			}
			titleBarInset={platform.titleBarInset}
		>
			<RecordControl recording={recording} />
			<nav aria-label="Filters" className="mt-3 flex flex-col gap-1">
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
			</nav>
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
		</SidebarColumn>
	);
}
