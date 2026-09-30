import {
	CheckCircle2Icon,
	CircleAlertIcon,
	ClockIcon,
	InboxIcon,
	MicOffIcon,
	PhoneIcon,
	SettingsIcon,
	SmartphoneIcon,
	TimerIcon,
	UsersIcon,
} from "lucide-react";
import { type ReactNode, useMemo } from "react";
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
	SidebarRow,
	SplitButton,
} from "@/components/ui";
import { cn } from "@/lib/cn";
import { format } from "./format";
import { useElapsedSeconds, useNow } from "./use-now";

type Filter = MeetingsListSnapshot["filter"];
type PermissionKind = RecordingSnapshot["deniedPermissions"][number];

const FILTERS: readonly { id: Filter; label: string; icon: ReactNode }[] = [
	{ id: "all", label: "All", icon: <InboxIcon /> },
	{ id: "processing", label: "In progress", icon: <ClockIcon /> },
	{ id: "ready", label: "Ready", icon: <CheckCircle2Icon /> },
	{ id: "failed", label: "Failed", icon: <CircleAlertIcon /> },
];

/** What a denied permission stops Steno from doing, and what allows it. */
const DENIED_COPY: Record<
	PermissionKind,
	{ title: string; description: string }
> = {
	microphone: {
		title: "Steno can't use the microphone.",
		description: "Allow it in System Settings to record.",
	},
	systemAudio: {
		title: "Steno can't hear the other side of calls.",
		description: "Allow system audio recording in System Settings.",
	},
	calendar: {
		title: "Steno can't see your calendar.",
		description: "Allow calendar access in System Settings.",
	},
	localNetwork: {
		title: "Steno can't reach your iPhone.",
		description: "Allow local network access in System Settings.",
	},
};

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
				className="h-9 w-full justify-start"
				data-testid={state === "starting" ? "sidebar-record" : "sidebar-stop"}
				disabled={busy}
				onClick={busy ? undefined : () => send(client, "recording.stop")}
				size="lg"
				variant="primary"
			>
				<RecordMark />
				{state === "recording"
					? "Stop"
					: state === "starting"
						? "Starting…"
						: "Stopping…"}
				{state === "recording" ? (
					<span className="ml-auto font-mono font-normal text-[12.5px] tabular-nums">
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
			size="lg"
			variant="primary"
		>
			<RecordMark />
			Record
		</SplitButton>
	);
}

/**
 * The recorder is about to stop on its own (the call ended): the seconds
 * left, counted down from the snapshot's figure, and a way to keep going.
 */
function AutoStopNotice({
	autoStop,
}: {
	autoStop: NonNullable<RecordingSnapshot["autoStop"]>;
}) {
	const client = useBridge();
	// The host's figure is taken as of the moment it arrived; the page counts
	// down from there until a snapshot with a new figure resets it.
	const anchor = useMemo(
		() => ({ at: Date.now(), seconds: autoStop.remainingSeconds }),
		[autoStop.remainingSeconds],
	);
	const now = useNow(true);
	const remaining = anchor.seconds - (now - anchor.at) / 1000;
	const countdown = (
		<span className="font-mono tabular-nums">
			{format.countdown(remaining)}
		</span>
	);
	return (
		<Callout
			actions={
				<Button
					data-testid="keep-recording"
					onClick={() => send(client, "recording.keepGoing")}
					size="sm"
					variant="outline"
				>
					Keep recording
				</Button>
			}
			data-testid="auto-stop"
			description={
				/[.!?]$/.test(autoStop.reason) ? autoStop.reason : `${autoStop.reason}.`
			}
			icon={<TimerIcon aria-hidden="true" />}
			size="sm"
			title={<>Stops in {countdown}</>}
			variant="live"
		/>
	);
}

/** A warning or error from the recorder, kept until the user dismisses it. */
function RecorderMessage({
	message,
	kind,
}: {
	message: string;
	kind: "warning" | "error";
}) {
	const client = useBridge();
	return (
		<Callout
			actions={
				<Button
					data-testid="dismiss-recorder-message"
					onClick={() => send(client, "recording.clearMessages")}
					size="sm"
					variant="ghost"
				>
					Dismiss
				</Button>
			}
			data-testid={`recorder-${kind}`}
			icon={<CircleAlertIcon aria-hidden="true" />}
			size="sm"
			title={message}
			variant="warning"
		/>
	);
}

/** A permission the recorder needs and does not have. */
function DeniedPermission({ kind }: { kind: PermissionKind }) {
	const client = useBridge();
	const copy = DENIED_COPY[kind];
	return (
		<Callout
			actions={
				<Button
					data-testid={`fix-${kind}`}
					onClick={() => send(client, "system.openSystemSettings", { kind })}
					size="sm"
					variant="outline"
				>
					Fix in System Settings
				</Button>
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
 * auto-stop countdown, a warning or error, and any permission it is missing.
 */
function RecordControl({
	recording,
}: {
	recording: RecordingSnapshot | undefined;
}) {
	return (
		<div className="mb-3 flex flex-col gap-1.5">
			<RecordButton recording={recording} />
			{recording?.autoStop && recording.state === "recording" ? (
				<AutoStopNotice autoStop={recording.autoStop} />
			) : null}
			{recording?.error ? (
				<RecorderMessage kind="error" message={recording.error} />
			) : recording?.warning ? (
				<RecorderMessage kind="warning" message={recording.warning} />
			) : null}
			{recording?.deniedPermissions.map((kind) => (
				<DeniedPermission key={kind} kind={kind} />
			))}
		</div>
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
