import {
	AudioWaveformIcon,
	CheckCircle2Icon,
	CircleAlertIcon,
	ClockIcon,
	CopyIcon,
	EllipsisIcon,
	FolderIcon,
	InfoIcon,
	RefreshCwIcon,
	RotateCcwIcon,
	ShareIcon,
	SparklesIcon,
	TextAlignStartIcon,
	Trash2Icon,
} from "lucide-react";
import { type ReactNode, useContext, useEffect, useState } from "react";
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
	focusRingClass,
	Menu,
	MenuGroup,
	MenuGroupLabel,
	MenuItem,
	MenuPopup,
	MenuRadioGroup,
	MenuRadioItem,
	MenuSeparator,
	MenuTrigger,
	notify,
	RecordMark,
	ResultLine,
	Switch,
	Tabs,
	TabsList,
	TabsPanel,
	TabsTab,
} from "@/components/ui";
import { cn } from "@/lib/cn";
import { SHORTCUTS, usePlatform, useShortcut } from "@/lib/platform";
import { useCommitToast } from "@/lib/use-commit-toast";
import { useElapsedSeconds } from "@/lib/use-now";
import { deleteMeeting } from "./delete-meeting";
import { firstSentence, format, formatRetention, formatSource } from "./format";
import { MeetingsToggle, NarrowLayoutContext } from "./narrow-layout";
import { NotesTab } from "./notes-tab";
import { ProcessingCard } from "./processing-card";
import { RecorderBar } from "./recorder-notices";
import { DIARIZE_STAGE } from "./speaker-names";
import { SpeakersPopover } from "./speakers-popover";
import { SummaryTab } from "./summary-tab";
import { TagEditor } from "./tag-editor";
import { TasksTab } from "./tasks-tab";
import { TranscriptTab, transcriptText } from "./transcript-tab";

type Tab = MeetingDetailSnapshot["tab"];
const TABS: readonly Tab[] = detailTab.options;

function isTab(value: unknown): value is Tab {
	return detailTab.safeParse(value).success;
}

/**
 * The speaker the `picker` flag opens the transcript's picker on: the first
 * unconfirmed one who speaks in the transcript, else the first unconfirmed
 * one.
 */
function firstPickable(detail: MeetingDetailSnapshot) {
	const unconfirmed = detail.speakers.filter(
		(speaker) => speaker.assignment !== "confirmed" && !speaker.dismissed,
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

/**
 * A one-time notice (`isNotice`) at the top of the reading column, which
 * the host records as seen once it is dismissed with OK. Setup that is
 * still open is not a notice: `SetupLine` says it once, quietly, in the
 * summary tab.
 */
function SetupBanner({ banner }: { banner: AppSnapshot["setupBanner"] }) {
	const client = useBridge();
	if (!banner?.isNotice) {
		return null;
	}
	return (
		<Callout
			actions={
				<Button
					data-testid="banner-not-now"
					onClick={() => send(client, "setup.dismissBanner")}
					size="xs"
					variant="outline"
				>
					OK
				</Button>
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

/** A text link on a muted line: the words in full colour, underlined. */
const lineLinkClass = cn(
	"rounded-xs text-foreground underline underline-offset-2 hover:no-underline",
	focusRingClass,
);

/**
 * Summaries or export still need setting up: one muted line over the
 * summary, led by the info glyph, with Set up (Settings at the section that
 * needs it) and Not now, both the line's underlined link, Not now a little
 * further off. Nothing on the other tabs.
 */
function SetupLine({ banner }: { banner: AppSnapshot["setupBanner"] }) {
	const client = useBridge();
	if (!banner || banner.isNotice) {
		return null;
	}
	const section = banner.offersSummaries
		? "summaries"
		: banner.offersVault
			? "export"
			: null;
	return (
		<div
			className="mt-0 mb-6 flex flex-wrap items-center gap-x-2 gap-y-1 text-body text-muted-foreground"
			data-testid="setup-line"
		>
			<InfoIcon aria-hidden="true" className="size-4 shrink-0 text-info" />
			<span className="min-w-0">
				{banner.title}{" "}
				{section ? (
					<button
						className={lineLinkClass}
						data-testid={
							section === "summaries" ? "setup-summaries" : "choose-vault"
						}
						onClick={() =>
							send(client, "window.open", { window: "settings", section })
						}
						type="button"
					>
						Set up
					</button>
				) : null}
			</span>
			<button
				className={cn(lineLinkClass, "ml-1")}
				data-testid="banner-not-now"
				onClick={() => send(client, "setup.dismissBanner")}
				type="button"
			>
				Not now
			</button>
		</div>
	);
}

/**
 * The header row's live mark while the recorder holds this meeting in a
 * wide window: the pulsing mark, "Recording" and the elapsed time, not a
 * control. The one Stop is the sidebar's (a narrow window's is in the
 * recorder bar). In a header row under 36 rem (a 960 px window) the word
 * goes and the mark and the time stay, so the breadcrumb keeps its words;
 * a screen reader still hears "Recording".
 */
function HeaderLive({ recording }: { recording: RecordingSnapshot }) {
	const elapsed = useElapsedSeconds(
		recording.state === "recording" ? recording.startedAt : undefined,
	);
	const stopping = recording.state === "stopping";
	return (
		<span
			className="flex shrink-0 items-center gap-2 text-foreground text-label"
			data-testid="header-live"
		>
			<RecordMark pulse={!stopping} />
			{stopping ? (
				"Stopping…"
			) : (
				<span className="@max-[36rem]/header:sr-only">Recording</span>
			)}
			{!stopping ? (
				<span className="font-mono font-normal text-meta text-muted-foreground tabular-nums">
					{format.duration(elapsed)}
				</span>
			) : null}
		</span>
	);
}

/**
 * "Process again" runs the meeting from the start with its recording; the
 * host says when it is offered (`canProcessAgain`) and words any refusal on
 * the error line. The button stays disabled until the host answers, so a
 * second click cannot start a second run, and the toast shows only when the
 * host confirms a run started (not on a declined prompt or a refusal). It
 * sits in the header row, or on the failure line when processing failed.
 */
function ProcessAgainButton({ disabled }: { disabled: boolean }) {
	const client = useBridge();
	const [starting, setStarting] = useState(false);
	async function processAgain() {
		setStarting(true);
		try {
			const { confirmed } = await client.call("meeting.processAgain");
			if (confirmed) {
				notify({ title: "Processing the meeting again." });
			}
		} catch (cause: unknown) {
			console.error("bridge: meeting.processAgain failed", cause);
		} finally {
			setStarting(false);
		}
	}
	return (
		<Button
			data-testid="process-again"
			disabled={disabled || starting}
			onClick={processAgain}
			size="sm"
			variant="outline"
		>
			<RotateCcwIcon aria-hidden="true" />
			Process again
		</Button>
	);
}

/**
 * Processing failed: a red result line in the tab's place, with the one
 * action that recovers from where it failed. With no transcript the
 * transcription failed, and only Process again (the whole run) helps; with
 * a transcript the summary failed, and Re-run summary is enough, or
 * Process again where the host does not allow that.
 */
function FailureLine({
	detail,
	onRerunSummary,
}: {
	detail: MeetingDetailSnapshot;
	onRerunSummary: () => void;
}) {
	const summaryFailed = detail.transcript.length > 0;
	let action: ReactNode;
	if (summaryFailed && detail.canRerunSummary) {
		action = (
			<Button
				data-testid="failure-rerun-summary"
				disabled={detail.isBusy}
				onClick={onRerunSummary}
				size="sm"
				variant="outline"
			>
				<RefreshCwIcon aria-hidden="true" />
				Re-run summary
			</Button>
		);
	} else if (detail.canProcessAgain) {
		action = <ProcessAgainButton disabled={detail.isBusy} />;
	}
	return (
		<ResultLine
			actions={action}
			data-testid="processing-failed"
			tone="destructive"
		>
			<span className="font-medium">Processing failed.</span>{" "}
			{detail.failureReason
				? firstSentence(detail.failureReason)
				: "No reason was reported."}
		</ResultLine>
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
		<span className="flex shrink-0 items-center gap-2 text-body text-foreground">
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

/** Where the export stands, as a result line: a tint once it ran. */
const EXPORT_RESULT: Record<
	MeetingDetailSnapshot["export"]["status"],
	{ icon: ReactNode; tone: "muted" | "success" | "destructive" }
> = {
	notConfigured: { icon: <ShareIcon aria-hidden="true" />, tone: "muted" },
	pending: { icon: <ClockIcon aria-hidden="true" />, tone: "muted" },
	delivered: { icon: <CheckCircle2Icon aria-hidden="true" />, tone: "success" },
	failed: { icon: <CircleAlertIcon aria-hidden="true" />, tone: "destructive" },
};

/**
 * The foot of the reading column: what happens to the recording, with the
 * keep switch when the host offers it, and, once the meeting is ready or
 * failed, where the export stands, with Reveal in Finder (the platform's
 * file manager) and Export again when they apply. While it records or is
 * processed there is nothing to export yet, so no export line.
 */
function DetailFooter({
	detail,
	onExport,
}: {
	detail: MeetingDetailSnapshot;
	onExport: () => void;
}) {
	const client = useBridge();
	const { words } = usePlatform();
	const status = EXPORT_RESULT[detail.export.status];
	const settled = detail.state === "ready" || detail.state === "failed";
	// Without the keep row or the export line the footer would be a lone rule.
	if (!detail.retention.showsKeepToggle && !settled) return null;
	return (
		<footer
			className="mt-10 flex flex-col gap-3 border-border border-t pt-4 text-body text-muted-foreground"
			data-testid="meeting-footer"
		>
			{detail.retention.showsKeepToggle ? (
				<div className="flex items-center gap-3" data-testid="retention-row">
					<AudioWaveformIcon
						aria-hidden="true"
						className="size-4 shrink-0 text-muted-foreground"
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
			{settled ? (
				<ResultLine
					actions={
						detail.export.canReveal || detail.export.canReexport ? (
							<>
								{detail.export.canReveal ? (
									<Button
										data-testid="reveal-export"
										onClick={() => send(client, "meeting.revealExport")}
										size="sm"
										variant="ghost"
									>
										<FolderIcon aria-hidden="true" />
										{words.revealInFileManager}
									</Button>
								) : null}
								{detail.export.canReexport ? (
									<Button
										data-testid="export-again"
										disabled={detail.isBusy}
										onClick={onExport}
										size="sm"
										variant="outline"
									>
										<ShareIcon aria-hidden="true" />
										Export again
									</Button>
								) : null}
							</>
						) : undefined
					}
					data-testid="export-status"
					icon={status.icon}
					tone={status.tone}
				>
					{detail.export.message}
				</ResultLine>
			) : null}
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
		<main className="relative flex min-h-0 min-w-0 flex-col">
			{hasSelection && detail ? (
				<DetailBody
					detail={detail}
					initialMenuOpen={initialMenuOpen}
					initialPickerOpen={initialPickerOpen}
					key={detail.id}
					setupBanner={app?.setupBanner}
				/>
			) : list && !hasSelection ? (
				<ContentColumn
					bar={<RecorderBar />}
					crumbs={["Meetings"]}
					leading={<MeetingsToggle />}
				>
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
	const platform = usePlatform();
	const recording = useSnapshot("recording");
	const progress = useSnapshot("progress");
	const [tab, setTab] = useState<Tab>(detail.tab);
	const [menuOpen, setMenuOpen] = useState(initialMenuOpen);
	const [pickerSpeakerID] = useState(() =>
		initialPickerOpen ? firstPickable(detail)?.id : undefined,
	);

	useEffect(() => {
		setTab(detail.tab);
	}, [detail.tab]);

	function changeTab(next: Tab) {
		setTab(next);
		send(client, "meeting.setTab", { tab: next });
	}

	// A narrow window's recorder bar holds Stop for every meeting.
	const { narrow } = useContext(NarrowLayoutContext);
	const entry = progress?.entries.find((item) => item.meetingID === detail.id);
	const working = detail.state === "processing" || detail.state === "queued";
	const holdsRecorder =
		recording !== undefined &&
		recording.meetingID === detail.id &&
		(recording.state === "recording" || recording.state === "stopping");

	function openSummaries() {
		send(client, "window.open", { window: "settings", section: "summaries" });
	}

	function rerun() {
		send(client, "meeting.rerunSummary");
	}

	function copyTranscript() {
		navigator.clipboard
			?.writeText(transcriptText(detail.transcript))
			.then(() => notify({ title: "Transcript copied." }))
			.catch((cause: unknown) => {
				console.error("copy transcript failed", cause);
			});
	}

	const canReexport = detail.export.canReexport && !detail.isBusy;
	const startExport = useCommitToast(
		detail.export.status === "delivered",
		detail.export.status === "failed",
		{ title: "Exported.", description: detail.export.message },
	);
	function reexport() {
		startExport();
		send(client, "meeting.reexport");
	}
	useShortcut(SHORTCUTS.exportAgain, reexport, canReexport);

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
		// Failed with a transcript: the summary failed, so the summary and its
		// tasks show the red line and the transcript stays readable.
		const summaryFailed = detail.state === "failed";
		const failure = <FailureLine detail={detail} onRerunSummary={rerun} />;
		switch (current) {
			case "summary":
				if (summaryFailed) {
					return failure;
				}
				return (
					<>
						<SetupLine banner={setupBanner} />
						<SummaryTab
							detail={detail}
							onRerunSummary={rerun}
							onSetUpSummaries={openSummaries}
						/>
					</>
				);
			case "transcript":
				return (
					<TranscriptTab
						detail={detail}
						initialPickerSpeakerID={pickerSpeakerID}
					/>
				);
			case "tasks":
				if (summaryFailed && detail.tasks.length === 0) {
					return failure;
				}
				return <TasksTab tasks={detail.tasks} />;
		}
	}

	const actions = (
		<div className="flex shrink-0 items-center gap-2">
			{holdsRecorder && recording && !narrow ? (
				<HeaderLive recording={recording} />
			) : null}
			{detail.canProcessAgain && detail.state !== "failed" ? (
				<ProcessAgainButton disabled={detail.isBusy} />
			) : null}
			<Button
				data-testid="export-meeting"
				disabled={!canReexport}
				onClick={reexport}
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
					{detail.templates.length > 0 ? (
						<>
							<MenuGroup>
								<MenuGroupLabel>Summary template</MenuGroupLabel>
								<MenuRadioGroup
									data-testid="template-select"
									disabled={detail.isBusy}
									onValueChange={(templateID: unknown) => {
										if (
											typeof templateID === "string" &&
											templateID !== detail.templateID
										) {
											send(client, "meeting.setTemplate", { templateID });
										}
									}}
									value={detail.templateID}
								>
									{detail.templates.map((template) => (
										<MenuRadioItem key={template.id} value={template.id}>
											{template.name}
										</MenuRadioItem>
									))}
								</MenuRadioGroup>
							</MenuGroup>
							<MenuSeparator />
						</>
					) : null}
					<MenuItem
						disabled={!detail.canRerunSummary || detail.isBusy}
						icon={<RefreshCwIcon />}
						onClick={rerun}
					>
						Re-run summary
					</MenuItem>
					<MenuSeparator />
					<MenuItem
						data-testid="copy-transcript"
						disabled={detail.transcript.length === 0}
						icon={<CopyIcon />}
						onClick={copyTranscript}
					>
						Copy transcript
					</MenuItem>
					<MenuItem
						disabled={!canReexport}
						icon={<ShareIcon />}
						onClick={reexport}
						shortcut={platform.label(SHORTCUTS.exportAgain)}
					>
						Export again
					</MenuItem>
					{detail.export.canReveal ? (
						<MenuItem
							icon={<FolderIcon />}
							onClick={() => send(client, "meeting.revealExport")}
						>
							{platform.words.showExport}
						</MenuItem>
					) : null}
					<MenuItem
						disabled={!detail.retention.filesExist}
						icon={<FolderIcon />}
						onClick={() => send(client, "meeting.revealRecording")}
					>
						{platform.words.showRecording}
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
		<ContentColumn
			actions={actions}
			bar={<RecorderBar />}
			crumbs={["Meetings", detail.title]}
			leading={<MeetingsToggle />}
		>
			<SetupBanner banner={setupBanner} />
			{detail.error ? (
				<Callout
					className="mb-6"
					icon={<CircleAlertIcon aria-hidden="true" />}
					title={detail.error}
					variant="destructive"
				/>
			) : null}
			{detail.audioWarning ? (
				<Callout
					className="mb-6"
					data-testid="audio-warning"
					icon={<CircleAlertIcon aria-hidden="true" />}
					title={detail.audioWarning}
					variant="warning"
				/>
			) : null}
			<h2 className="m-0 text-display" data-testid="meeting-title">
				{detail.title}
			</h2>
			{/*
			 * One meta line under the title: source, date and length. Each
			 * item after the first leads with its separator dot, so a narrow
			 * column wraps the dot with its item instead of leaving it at the
			 * end of the line, and no item breaks inside itself ("Tuesday,
			 * Sep 29 · 14:50" stays whole at 200 % text). The language and the
			 * retention are not here: the footer says what happens to the
			 * recording.
			 */}
			<div
				className="mt-1 mb-4 flex flex-wrap items-center gap-x-2 gap-y-1 text-meta text-muted-foreground [&>span+span]:before:size-[3px] [&>span+span]:before:rounded-full [&>span+span]:before:bg-border [&>span+span]:before:content-[''] [&>span]:inline-flex [&>span]:items-center [&>span]:gap-2 [&>span]:whitespace-nowrap"
				data-testid="meeting-meta"
			>
				<span>{formatSource(detail.source)}</span>
				<span>{format.when(detail.startedAt)}</span>
				{detail.state === "recording" ? null : (
					// Not known until the recording stops; the header shows the
					// elapsed time meanwhile.
					<span className="tabular-nums">
						{format.duration(detail.durationSeconds)}
					</span>
				)}
			</div>
			<div
				className="@container/speakers mb-5 flex flex-wrap items-center gap-2"
				data-testid="speakers-row"
			>
				<SpeakersPopover
					finding={entry?.stage === DIARIZE_STAGE}
					speakers={detail.speakers}
					transcript={detail.transcript}
				/>
				{detail.tags.map((tag) => (
					<Badge key={tag}>#{tag}</Badge>
				))}
				<TagEditor tags={detail.tags} />
			</div>

			{detail.state === "failed" && detail.transcript.length === 0 ? (
				// Transcription failed: the summary, transcript and tasks would
				// be empty, so the red line with Process again takes their place,
				// and the personal notes stay below it, still editable.
				<>
					<FailureLine detail={detail} onRerunSummary={rerun} />
					<section
						aria-labelledby="failed-notes-title"
						className="mt-8"
						data-testid="failed-notes"
					>
						<h3
							className="m-0 mb-3 text-foreground text-label"
							id="failed-notes-title"
						>
							Notes
						</h3>
						<NotesTab meetingID={detail.id} notes={detail.notes} />
					</section>
				</>
			) : (
				<Tabs
					onValueChange={(value) => {
						if (isTab(value)) {
							changeTab(value);
						}
					}}
					value={tab}
				>
					<div className="mb-6 flex min-w-0">
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
					</div>
					{TABS.map((item) => (
						<TabsPanel key={item} value={item} variant="reading">
							{tab === item ? panel(item) : null}
						</TabsPanel>
					))}
				</Tabs>
			)}
			<DetailFooter detail={detail} onExport={reexport} />
		</ContentColumn>
	);
}
