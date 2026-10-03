import {
	AudioWaveformIcon,
	CheckCircle2Icon,
	CircleAlertIcon,
	ClockIcon,
	EllipsisIcon,
	FolderIcon,
	RefreshCwIcon,
	ShareIcon,
	SparklesIcon,
	TextAlignStartIcon,
	Trash2Icon,
} from "lucide-react";
import { type ReactNode, useEffect, useState } from "react";
import type {
	AppSnapshot,
	MeetingDetailSnapshot,
	RecordingSnapshot,
} from "@/bridge/contract";
import { detailTab } from "@/bridge/contract";
import { send, useBridge, useSnapshot } from "@/bridge/hooks";
import {
	Badge,
	Button,
	Callout,
	ContentColumn,
	EmptyState,
	Menu,
	MenuItem,
	MenuPopup,
	MenuSeparator,
	MenuTrigger,
	RecordMark,
	Select,
	StatusIcon,
	Switch,
	Tabs,
	TabsList,
	TabsPanel,
	TabsTab,
} from "@/components/ui";
import { cn } from "@/lib/cn";
import { useElapsedSeconds } from "@/lib/use-now";
import { deleteMeeting } from "./delete-meeting";
import {
	firstSentence,
	format,
	formatPeople,
	formatRetention,
	formatSource,
} from "./format";
import { NotesTab } from "./notes-tab";
import { ProcessingCard } from "./processing-card";
import { SOURCE } from "./source";
import { SpeakersPopover } from "./speakers-popover";
import { SummaryTab } from "./summary-tab";
import { TagEditor } from "./tag-editor";
import { TasksTab } from "./tasks-tab";
import { TranscriptTab } from "./transcript-tab";

type Tab = MeetingDetailSnapshot["tab"];
const TABS: readonly Tab[] = detailTab.options;

function isTab(value: unknown): value is Tab {
	return detailTab.safeParse(value).success;
}

/**
 * The speaker "Confirm speaker" opens the picker on: the first unconfirmed
 * one who speaks in the transcript, else the first unconfirmed one.
 */
function firstPickable(detail: MeetingDetailSnapshot) {
	const unconfirmed = detail.speakers.filter(
		(speaker) => speaker.assignment !== "confirmed",
	);
	return (
		unconfirmed.find((speaker) =>
			detail.transcript.some((turn) => turn.speakerID === speaker.id),
		) ?? unconfirmed[0]
	);
}

export interface MeetingDetailProps {
	/** Opens the actions menu on mount (the screens). */
	initialMenuOpen?: boolean;
	/** Opens the speaker picker on the first unconfirmed speaker on mount. */
	initialPickerOpen?: boolean;
}

/** The notice at the top of the reading column: summaries or export still need setting up. */
function SetupBanner({ banner }: { banner: AppSnapshot["setupBanner"] }) {
	const client = useBridge();
	if (!banner) {
		return null;
	}
	return (
		<Callout
			actions={
				<>
					{banner.offersSummaries ? (
						<Button
							data-testid="setup-summaries"
							onClick={() =>
								send(client, "window.open", {
									window: "settings",
									section: "summaries",
								})
							}
							size="xs"
							variant="primary"
						>
							Set up summaries
						</Button>
					) : null}
					{banner.offersVault ? (
						<Button
							data-testid="choose-vault"
							onClick={() =>
								send(client, "window.open", {
									window: "settings",
									section: "export",
								})
							}
							size="xs"
							variant={banner.offersSummaries ? "outline" : "primary"}
						>
							Choose vault
						</Button>
					) : null}
					<Button
						data-testid="banner-not-now"
						onClick={() => send(client, "setup.dismissBanner")}
						size="xs"
						variant="ghost"
					>
						Not now
					</Button>
				</>
			}
			className="mb-6"
			data-testid="setup-banner"
			description={banner.body}
			icon={<SparklesIcon aria-hidden="true" />}
			title={banner.title}
			variant="default"
		/>
	);
}

/** The Stop control in the header row while the recorder holds this meeting. */
function HeaderStop({ recording }: { recording: RecordingSnapshot }) {
	const client = useBridge();
	const elapsed = useElapsedSeconds(
		recording.state === "recording" ? recording.startedAt : undefined,
	);
	const stopping = recording.state === "stopping";
	return (
		<Button
			data-testid="header-stop"
			disabled={stopping}
			onClick={() => send(client, "recording.stop")}
			size="sm"
			variant="primary"
		>
			<RecordMark pulse={!stopping} />
			{stopping ? "Stopping…" : "Stop"}
			{!stopping ? (
				<span className="font-mono font-normal text-xs tabular-nums">
					{format.duration(elapsed)}
				</span>
			) : null}
		</Button>
	);
}

/**
 * "Keep the recording": the switch follows the host, shows the new position
 * at once, and slides back when the host's alert was declined.
 */
function KeepAudioToggle({
	keepsAudio,
	disabled,
}: {
	keepsAudio: boolean;
	disabled: boolean;
}) {
	const client = useBridge();
	const [keeps, setKeeps] = useState(keepsAudio);

	useEffect(() => {
		setKeeps(keepsAudio);
	}, [keepsAudio]);

	async function change(next: boolean) {
		const previous = keeps;
		setKeeps(next);
		try {
			const reply = await client.call("meeting.setKeepAudio", { value: next });
			if (!reply.confirmed) {
				setKeeps(previous);
			}
		} catch (cause: unknown) {
			console.error("bridge: meeting.setKeepAudio failed", cause);
			setKeeps(previous);
		}
	}

	return (
		<span className="flex shrink-0 items-center gap-2 text-foreground text-sm">
			<Switch
				aria-label="Keep the recording"
				checked={keeps}
				data-testid="keep-audio"
				disabled={disabled}
				onCheckedChange={(next) => {
					void change(next);
				}}
			/>
			Keep the recording
		</span>
	);
}

const EXPORT_ICON: Record<
	MeetingDetailSnapshot["export"]["status"],
	{ icon: ReactNode; tone: "faint" | "muted" | "success" | "warning" }
> = {
	notConfigured: { icon: <ShareIcon />, tone: "faint" },
	pending: { icon: <ClockIcon />, tone: "muted" },
	delivered: { icon: <CheckCircle2Icon />, tone: "success" },
	failed: { icon: <CircleAlertIcon />, tone: "warning" },
};

/**
 * The foot of the reading column: what happens to the recording, with the
 * keep switch when the host offers it, and where the export stands, with
 * Reveal in Finder and Export again when they apply.
 */
function DetailFooter({ detail }: { detail: MeetingDetailSnapshot }) {
	const client = useBridge();
	const status = EXPORT_ICON[detail.export.status];
	return (
		<footer
			className="mt-10 flex flex-col gap-3 border-border border-t pt-4 text-muted-foreground text-sm"
			data-testid="meeting-footer"
		>
			{detail.retention.showsKeepToggle ? (
				<div className="flex items-center gap-3" data-testid="retention-row">
					<AudioWaveformIcon
						aria-hidden="true"
						className="size-4 shrink-0 text-faint"
					/>
					<span className="min-w-0 flex-1">
						{formatRetention(detail.retention)}.
					</span>
					<KeepAudioToggle
						disabled={detail.isBusy}
						keepsAudio={detail.retention.keepsAudio}
					/>
				</div>
			) : null}
			<div
				className="flex flex-wrap items-center gap-x-3 gap-y-2"
				data-testid="export-status"
			>
				<StatusIcon tone={status.tone}>{status.icon}</StatusIcon>
				<span className="min-w-0 flex-1">{detail.export.message}</span>
				{detail.export.canReveal ? (
					<Button
						data-testid="reveal-export"
						onClick={() => send(client, "meeting.revealExport")}
						size="sm"
						variant="ghost"
					>
						<FolderIcon aria-hidden="true" />
						Reveal in Finder
					</Button>
				) : null}
				{detail.export.canReexport ? (
					<Button
						data-testid="export-again"
						disabled={detail.isBusy}
						onClick={() => send(client, "meeting.reexport")}
						size="sm"
						variant="outline"
					>
						<ShareIcon aria-hidden="true" />
						Export again
					</Button>
				) : null}
			</div>
		</footer>
	);
}

/**
 * The reading column for the selected meeting: the header row with the
 * breadcrumb, Export and the actions menu, then the setup banner, the
 * eyebrow, title, people and tags, the tabs and the footer. Processing and
 * failure replace the tab content; the notes stay editable throughout.
 */
export function MeetingDetail({
	initialMenuOpen = false,
	initialPickerOpen = false,
}: MeetingDetailProps) {
	const app = useSnapshot("app");
	const list = useSnapshot("meetings.list");
	const detail = useSnapshot("meeting.detail");
	const hasSelection = list?.selection !== undefined;

	return (
		<main className="surface-grain relative flex min-h-0 min-w-0 flex-col">
			{hasSelection && detail ? (
				<DetailBody
					detail={detail}
					initialMenuOpen={initialMenuOpen}
					initialPickerOpen={initialPickerOpen}
					key={detail.id}
					setupBanner={app?.setupBanner}
				/>
			) : list && !hasSelection ? (
				<ContentColumn crumbs={["Meetings"]}>
					<SetupBanner banner={app?.setupBanner} />
					<EmptyState
						body="Pick a meeting on the left to read its summary, transcript and tasks."
						className="mt-24"
						icon={<TextAlignStartIcon aria-hidden="true" />}
						id="empty-detail"
						size="lg"
						title="Select a meeting"
					/>
				</ContentColumn>
			) : null}
		</main>
	);
}

function DetailBody({
	detail,
	setupBanner,
	initialMenuOpen,
	initialPickerOpen,
}: {
	detail: MeetingDetailSnapshot;
	setupBanner: AppSnapshot["setupBanner"];
	initialMenuOpen: boolean;
	initialPickerOpen: boolean;
}) {
	const client = useBridge();
	const recording = useSnapshot("recording");
	const progress = useSnapshot("progress");
	const [tab, setTab] = useState<Tab>(detail.tab);
	const [menuOpen, setMenuOpen] = useState(initialMenuOpen);
	const [pickerRequest, setPickerRequest] = useState<
		{ speakerID: string; nonce: number } | undefined
	>(() => {
		const first = initialPickerOpen ? firstPickable(detail) : undefined;
		return first ? { speakerID: first.id, nonce: 0 } : undefined;
	});

	useEffect(() => {
		setTab(detail.tab);
	}, [detail.tab]);

	function changeTab(next: Tab) {
		setTab(next);
		send(client, "meeting.setTab", { tab: next });
	}

	const unconfirmed = detail.speakers.filter(
		(speaker) => speaker.assignment !== "confirmed",
	);
	const entry = progress?.entries.find((item) => item.meetingID === detail.id);
	const working = detail.state === "processing" || detail.state === "queued";
	const holdsRecorder =
		recording !== undefined &&
		recording.meetingID === detail.id &&
		(recording.state === "recording" || recording.state === "stopping");

	function confirmSpeaker() {
		const first = firstPickable(detail);
		if (!first) {
			return;
		}
		changeTab("transcript");
		setPickerRequest((previous) => ({
			speakerID: first.id,
			nonce: (previous?.nonce ?? 0) + 1,
		}));
	}

	function openSummaries() {
		send(client, "window.open", { window: "settings", section: "summaries" });
	}

	function rerun() {
		send(client, "meeting.rerunSummary");
	}

	function panel(current: Tab) {
		if (current === "notes") {
			return (
				<>
					{working ? (
						<ProcessingCard entry={entry} state={detail.state} />
					) : null}
					<NotesTab meetingID={detail.id} notes={detail.notes} />
				</>
			);
		}
		if (detail.state === "recording") {
			return (
				<EmptyState
					body="The summary, transcript and tasks appear a few minutes after you stop."
					icon={<AudioWaveformIcon aria-hidden="true" />}
					id={`empty-${current}`}
					title="Recording"
				/>
			);
		}
		if (working) {
			return <ProcessingCard entry={entry} state={detail.state} />;
		}
		if (detail.state === "failed") {
			return (
				<EmptyState
					action={
						<Button
							data-testid={`${current}-try-again`}
							disabled={!detail.canRerunSummary || detail.isBusy}
							onClick={rerun}
							variant="outline"
						>
							<RefreshCwIcon aria-hidden="true" />
							Try again
						</Button>
					}
					body={
						detail.failureReason
							? firstSentence(detail.failureReason)
							: "No reason was reported."
					}
					icon={<CircleAlertIcon aria-hidden="true" />}
					id={`empty-${current}`}
					title="Processing failed"
					variant="warning"
				/>
			);
		}
		switch (current) {
			case "summary":
				return (
					<SummaryTab
						detail={detail}
						onRerunSummary={rerun}
						onSetUpSummaries={openSummaries}
					/>
				);
			case "transcript":
				return <TranscriptTab detail={detail} pickerRequest={pickerRequest} />;
			case "tasks":
				return <TasksTab tasks={detail.tasks} />;
		}
	}

	const actions = (
		<div className="flex shrink-0 items-center gap-2">
			{holdsRecorder && recording ? <HeaderStop recording={recording} /> : null}
			<Button
				data-testid="export-meeting"
				disabled={!detail.export.canReexport || detail.isBusy}
				onClick={() => send(client, "meeting.reexport")}
				size="sm"
				variant="outline"
			>
				<ShareIcon aria-hidden="true" />
				Export
			</Button>
			<Menu onOpenChange={setMenuOpen} open={menuOpen}>
				<MenuTrigger
					data-testid="meeting-actions"
					render={
						<Button aria-label="More actions" size="icon-sm" variant="ghost" />
					}
				>
					<EllipsisIcon aria-hidden="true" />
				</MenuTrigger>
				<MenuPopup align="end" sideOffset={4}>
					<MenuItem
						disabled={!detail.canRerunSummary || detail.isBusy}
						icon={<RefreshCwIcon />}
						onClick={rerun}
					>
						Re-run summary
					</MenuItem>
					<MenuItem
						disabled={!detail.export.canReexport || detail.isBusy}
						icon={<ShareIcon />}
						onClick={() => send(client, "meeting.reexport")}
						shortcut="⇧⌘E"
					>
						Export again
					</MenuItem>
					{detail.export.canReveal ? (
						<MenuItem
							icon={<FolderIcon />}
							onClick={() => send(client, "meeting.revealExport")}
						>
							Reveal export
						</MenuItem>
					) : null}
					<MenuItem
						disabled={!detail.retention.filesExist}
						icon={<FolderIcon />}
						onClick={() => send(client, "meeting.revealRecording")}
					>
						Reveal recording
					</MenuItem>
					<MenuSeparator />
					{detail.retention.filesExist ? (
						<MenuItem
							data-testid="delete-recording"
							disabled={detail.isBusy}
							icon={<AudioWaveformIcon />}
							onClick={() => send(client, "meeting.deleteRecordingNow")}
							variant="destructive"
						>
							Delete recording now…
						</MenuItem>
					) : null}
					<MenuItem
						data-testid="delete-meeting"
						icon={<Trash2Icon />}
						onClick={() => deleteMeeting(client, detail.id)}
						variant="destructive"
					>
						Delete meeting…
					</MenuItem>
				</MenuPopup>
			</Menu>
		</div>
	);

	return (
		<ContentColumn actions={actions} crumbs={["Meetings", detail.title]}>
			<SetupBanner banner={setupBanner} />
			{detail.error ? (
				<Callout
					className="mb-6"
					icon={<CircleAlertIcon aria-hidden="true" />}
					title={detail.error}
					variant="destructive"
				/>
			) : null}
			<div
				className="flex flex-wrap items-center gap-2 text-muted-foreground text-xs [&>i]:size-[3px] [&>i]:rounded-full [&>i]:bg-border"
				data-testid="meeting-meta"
			>
				<span
					className={cn(
						"inline-flex items-center gap-[5px] font-medium text-muted-foreground before:size-1.5 before:rounded-full before:content-['']",
						SOURCE[detail.source].dot,
					)}
				>
					{formatSource(detail.source)}
				</span>
				<i />
				<span>{format.when(detail.startedAt)}</span>
				<i />
				<span className="tabular-nums">
					{format.duration(detail.durationSeconds)}
				</span>
				{detail.language ? (
					<>
						<i />
						<span>{format.language(detail.language)}</span>
					</>
				) : null}
				<i />
				<span>{formatRetention(detail.retention)}</span>
			</div>
			<h2
				className="mt-2 mb-3 font-semibold text-2xl leading-tight tracking-tight"
				data-testid="meeting-title"
			>
				{detail.title}
			</h2>
			<div
				className="mb-6 flex flex-wrap items-center gap-2"
				data-testid="speakers-row"
			>
				{detail.speakers.length > 0 ? (
					<SpeakersPopover speakers={detail.speakers} />
				) : (
					<span className="text-muted-foreground text-sm">
						{formatPeople(detail.speakers)}
					</span>
				)}
				{unconfirmed.length > 0 && detail.transcript.length > 0 ? (
					<Button
						data-testid="confirm-speaker"
						onClick={confirmSpeaker}
						size="xs"
						variant="warning-outline"
					>
						<CheckCircle2Icon aria-hidden="true" />
						{unconfirmed.length === 1 ? "Confirm speaker" : "Confirm speakers"}
					</Button>
				) : null}
				{detail.tags.map((tag) => (
					<Badge key={tag}>#{tag}</Badge>
				))}
				<TagEditor tags={detail.tags} />
			</div>

			<Tabs
				onValueChange={(value) => {
					if (isTab(value)) {
						changeTab(value);
					}
				}}
				value={tab}
			>
				<div className="mb-6 flex flex-wrap items-center justify-between gap-3">
					<TabsList>
						<TabsTab data-testid="tab-summary" value="summary">
							Summary
						</TabsTab>
						<TabsTab
							count={detail.transcript.length || undefined}
							data-testid="tab-transcript"
							value="transcript"
						>
							Transcript
						</TabsTab>
						<TabsTab
							count={detail.tasks.length || undefined}
							data-testid="tab-tasks"
							value="tasks"
						>
							Tasks
						</TabsTab>
						<TabsTab data-testid="tab-notes" value="notes">
							Notes
						</TabsTab>
					</TabsList>
					{tab === "summary" && detail.templates.length > 0 ? (
						<span
							className="flex items-center gap-2 text-muted-foreground text-xs"
							data-testid="template-row"
						>
							Template
							<Select
								aria-label="Summary template"
								className="max-w-45"
								data-testid="template-select"
								disabled={detail.isBusy}
								onValueChange={(templateID) => {
									if (templateID && templateID !== detail.templateID) {
										send(client, "meeting.setTemplate", { templateID });
									}
								}}
								options={detail.templates.map((template) => ({
									value: template.id,
									label: template.name,
								}))}
								size="xs"
								value={detail.templateID}
							/>
						</span>
					) : null}
				</div>
				{TABS.map((item) => (
					<TabsPanel key={item} value={item} variant="reading">
						{tab === item ? panel(item) : null}
					</TabsPanel>
				))}
			</Tabs>
			<DetailFooter detail={detail} />
		</ContentColumn>
	);
}
