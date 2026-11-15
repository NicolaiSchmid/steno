import { AudioWaveformIcon, SearchIcon, Trash2Icon } from "lucide-react";
import {
	type ChangeEvent,
	type KeyboardEvent,
	useContext,
	useEffect,
	useRef,
	useState,
} from "react";
import type {
	MeetingRow,
	MeetingsListSnapshot,
	ProgressSnapshot,
} from "@/bridge/contract";
import { send, useBridge, useSnapshot } from "@/bridge/hooks";
import {
	Button,
	ContextMenu,
	ContextMenuItem,
	ContextMenuPopup,
	ContextMenuTrigger,
	EmptyState,
	HeaderRow,
	ScrollArea,
	SearchInput,
	SectionLabel,
} from "@/components/ui";
import { cn } from "@/lib/cn";
import {
	type Platform,
	type PlatformWords,
	SHORTCUTS,
	usePlatform,
	useShortcut,
} from "@/lib/platform";
import { deleteMeeting } from "./delete-meeting";
import { firstSentence, format } from "./format";
import { NarrowLayoutContext } from "./narrow-layout";
import { isParked } from "./processing-card";

export const QUERY_DEBOUNCE_MS = 200;

/**
 * The third line of a row when the host sent no preview. `live`: a row
 * still `recording` may be the recording in progress, which it is unless
 * the recorder is idle; one left `recording` while the recorder is idle is
 * one whose save failed, or that an earlier run left, and Steno recovers it
 * at its next start.
 */
export function rowPreview(
	row: MeetingRow,
	progress: ProgressSnapshot["entries"][number] | undefined,
	live: boolean,
): string {
	if (row.preview) {
		return row.preview;
	}
	switch (row.state) {
		case "recording":
			return live
				? "Recording now."
				: "Not saved yet. Steno will process it the next time it starts.";
		case "queued":
			return progress && isParked(progress)
				? `${progress.title}.`
				: "Waiting to process.";
		case "processing":
			// A run set aside whose meeting the store kept `processing`.
			if (progress && isParked(progress)) {
				return `${progress.title}.`;
			}
			return progress ? `${progress.title}…` : "Processing.";
		case "failed":
			return row.failureReason
				? firstSentence(row.failureReason)
				: "Processing failed.";
		case "ready":
			return row.hasSummary ? "Summary ready." : "Transcript ready.";
	}
}

/** " The tray icon works too." where the platform always has one. */
function trayHint({ trayItem }: PlatformWords): string {
	return trayItem ? ` The ${trayItem} works too.` : "";
}

function emptyCopy(
	list: MeetingsListSnapshot,
	platform: Platform,
): {
	title: string;
	body: string;
} {
	if (list.counts.all === 0) {
		return {
			title: "No meetings yet",
			body: `Press Record above, or ${platform.label(SHORTCUTS.record)}.${trayHint(platform.words)}`,
		};
	}
	if (list.query.trim()) {
		return {
			title: "No meetings match",
			body: "Nothing matches this search.",
		};
	}
	if (list.tagFilter) {
		return {
			title: "No meetings with this tag",
			body: `Nothing is tagged #${list.tagFilter} in this view.`,
		};
	}
	switch (list.filter) {
		case "processing":
			return {
				title: "Nothing in progress",
				body: "Meetings appear here while Steno is working on them.",
			};
		case "ready":
			return {
				title: "Nothing ready yet",
				body: "Finished meetings appear here.",
			};
		case "failed":
			return {
				title: "Nothing failed",
				body: "Meetings that could not be processed appear here.",
			};
		case "all":
			return {
				title: "No meetings match",
				body: "Nothing matches this search or filter.",
			};
	}
}

function flatIDs(list: MeetingsListSnapshot): string[] {
	return list.groups.flatMap((group) =>
		group.meetings.map((meeting) => meeting.id),
	);
}

/**
 * The 300 px column on the canvas, beside the sidebar with no rule between
 * them and a hairline before the reading column: the header row and the
 * search row, then the day groups with one two-line row per meeting. The selected row is filled and stays in view.
 * Right-click or the Delete key deletes behind the host's confirmation; the
 * arrow keys move the selection.
 */
export function MeetingList() {
	const client = useBridge();
	const platform = usePlatform();
	const list = useSnapshot("meetings.list");
	const progress = useSnapshot("progress");
	const recording = useSnapshot("recording");
	// Only an idle recorder proves a `recording` row is not live: the list
	// and the recording snapshot publish apart, so a row's own id can lag
	// the recorder's in either direction around a start or a stop.
	const live = recording?.state !== "idle";
	const [query, setQuery] = useState("");
	const pending = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
	const search = useRef<HTMLInputElement>(null);
	useShortcut(SHORTCUTS.findMeetings, () => search.current?.focus());
	const hostQuery = list?.query ?? "";

	useEffect(() => {
		if (pending.current === undefined) {
			setQuery(hostQuery);
		}
	}, [hostQuery]);

	useEffect(() => () => clearTimeout(pending.current), []);

	function onQueryChange(event: ChangeEvent<HTMLInputElement>) {
		const value = event.target.value;
		setQuery(value);
		clearTimeout(pending.current);
		pending.current = setTimeout(() => {
			pending.current = undefined;
			send(client, "meetings.setQuery", { query: value });
		}, QUERY_DEBOUNCE_MS);
	}

	function clearFilters() {
		clearTimeout(pending.current);
		pending.current = undefined;
		setQuery("");
		send(client, "meetings.setFilter", { filter: "all" });
		send(client, "meetings.setTagFilter", {});
		send(client, "meetings.setQuery", { query: "" });
	}

	function onKeyDown(event: KeyboardEvent<HTMLElement>) {
		if (!list || (event.target as HTMLElement).tagName === "INPUT") {
			return;
		}
		if (event.key === "Delete" || event.key === "Backspace") {
			if (list.selection) {
				event.preventDefault();
				deleteMeeting(client, list.selection);
			}
			return;
		}
		if (event.key !== "ArrowDown" && event.key !== "ArrowUp") {
			return;
		}
		const ids = flatIDs(list);
		const at = list.selection ? ids.indexOf(list.selection) : -1;
		const next = ids[event.key === "ArrowDown" ? at + 1 : at - 1];
		if (next) {
			event.preventDefault();
			send(client, "meetings.select", { meetingID: next });
		}
	}

	const entries = new Map(
		(progress?.entries ?? []).map((entry) => [entry.meetingID, entry]),
	);

	return (
		<section
			aria-label="Meetings"
			className="surface-canvas flex min-h-0 flex-col overflow-hidden border-border border-r bg-background"
			data-testid="meeting-list"
			onKeyDown={onKeyDown}
		>
			<HeaderRow inset="sm">
				<h1 className="m-0 min-w-0 truncate text-foreground text-title">
					Meetings
				</h1>
			</HeaderRow>
			<div className="flex flex-col gap-1 px-2 pb-1">
				<SearchInput
					aria-label="Search meetings"
					data-testid="search-meetings"
					onChange={onQueryChange}
					placeholder="Search meetings"
					ref={search}
					shortcut={platform.label(SHORTCUTS.findMeetings)}
					value={query}
					variant="row"
				/>
				{list?.error ? (
					<p className="my-0 px-2 text-destructive-foreground text-meta">
						{list.error}
					</p>
				) : null}
			</div>
			<ScrollArea className="flex-1">
				{list && list.groups.length === 0 ? (
					<EmptyState
						action={
							list.counts.all > 0 ? (
								<Button
									data-testid="clear-filters"
									onClick={clearFilters}
									size="sm"
									variant="outline"
								>
									Clear filters
								</Button>
							) : undefined
						}
						icon={
							list.counts.all === 0 ? (
								<AudioWaveformIcon aria-hidden="true" />
							) : (
								<SearchIcon aria-hidden="true" />
							)
						}
						id="empty-meetings"
						{...emptyCopy(list, platform)}
					/>
				) : null}
				{list?.groups.map((group) => {
					const day = format.dayLabel(group.day);
					const date = <span className="font-normal">{day.date}</span>;
					return (
						<div className="flex flex-col gap-px px-2" key={group.day}>
							<SectionLabel trailing={date}>{day.label}</SectionLabel>
							{group.meetings.map((meeting) => (
								<MeetingRowView
									active={meeting.id === list.selection}
									key={meeting.id}
									live={live}
									meeting={meeting}
									progress={entries.get(meeting.id)}
								/>
							))}
						</div>
					);
				})}
			</ScrollArea>
		</section>
	);
}

type RowState = Exclude<MeetingRow["state"], "ready">;

/** The state dot's colour; a ready meeting has none. */
const DOT: Record<RowState, string> = {
	recording: "bg-live",
	failed: "bg-destructive",
	processing: "bg-info",
	queued: "bg-info",
};

/** The state in words for a screen reader; the meta line says it to the eye. */
const LABEL: Record<RowState, string> = {
	recording: "Live",
	failed: "Failed",
	processing: "Working",
	queued: "Working",
};

/**
 * The dot before the title while a meeting is not simply done: red and
 * pulsing while it records, red once it failed, blue while it works, amber
 * for a recording left unsaved (not `live`), which Steno processes at its
 * next start. The preview line says the same in words. A ready meeting
 * keeps the empty slot, so every title starts at the same edge.
 */
function StateDot({ meeting, live }: { meeting: MeetingRow; live: boolean }) {
	if (meeting.state === "ready") {
		return <span aria-hidden="true" className="size-2" />;
	}
	const unsaved = meeting.state === "recording" && !live;
	return (
		<>
			<span
				aria-hidden="true"
				className={cn(
					"size-2 shrink-0 rounded-full",
					unsaved ? "bg-warning" : DOT[meeting.state],
					meeting.state === "recording" && live && "animate-status-pulse",
				)}
				data-testid={`meeting-state-${meeting.id}`}
			/>
			<span className="sr-only">
				{unsaved ? "Not processed" : LABEL[meeting.state]}:{" "}
			</span>
		</>
	);
}

/**
 * One meeting in two lines: the state dot and the title, then the start
 * time and the preview in the meta size, both lines after the dot's 8 px
 * slot.
 */
function MeetingRowView({
	meeting,
	active,
	live,
	progress,
}: {
	meeting: MeetingRow;
	active: boolean;
	live: boolean;
	progress: ProgressSnapshot["entries"][number] | undefined;
}) {
	const client = useBridge();
	const { closeMeetings } = useContext(NarrowLayoutContext);
	const row = useRef<HTMLButtonElement>(null);

	useEffect(() => {
		if (active) {
			row.current?.scrollIntoView?.({ block: "nearest" });
		}
	}, [active]);

	return (
		<ContextMenu>
			<ContextMenuTrigger className="block">
				<button
					aria-current={active ? "true" : undefined}
					className={cn(
						"relative block w-full rounded-md px-2.5 py-2 text-left outline-none transition-colors duration-(--duration-functional) ease-standard focus-visible:ring-2 focus-visible:ring-focus focus-visible:ring-inset",
						active ? "bg-row-selected text-foreground" : "hover:bg-row-hover",
					)}
					data-testid={`meeting-${meeting.id}`}
					onClick={() => {
						send(client, "meetings.select", { meetingID: meeting.id });
						closeMeetings();
					}}
					ref={row}
					type="button"
				>
					<div className="grid grid-cols-[0.5rem_minmax(0,1fr)] items-center gap-x-2">
						<StateDot live={live} meeting={meeting} />
						<span className="min-w-0 truncate text-foreground text-title">
							{meeting.title}
						</span>
						<div className="col-start-2 mt-0.5 flex min-w-0 items-center gap-1.5 text-meta text-muted-foreground">
							<time
								className="shrink-0 tabular-nums"
								dateTime={meeting.startedAt}
							>
								{format.time(meeting.startedAt)}
							</time>
							<span aria-hidden="true">·</span>
							<span className="min-w-0 flex-1 truncate">
								{rowPreview(meeting, progress, live)}
							</span>
						</div>
					</div>
				</button>
			</ContextMenuTrigger>
			<ContextMenuPopup>
				<ContextMenuItem
					icon={<Trash2Icon />}
					onClick={() => deleteMeeting(client, meeting.id)}
					variant="destructive"
				>
					Delete meeting…
				</ContextMenuItem>
			</ContextMenuPopup>
		</ContextMenu>
	);
}
