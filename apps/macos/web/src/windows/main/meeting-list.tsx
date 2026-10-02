import { AudioWaveformIcon, SearchIcon, Trash2Icon } from "lucide-react";
import {
	type ChangeEvent,
	type KeyboardEvent,
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
	Avatar,
	AvatarStack,
	Badge,
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
import { deleteMeeting } from "./delete-meeting";
import { firstSentence, format, formatSource } from "./format";
import { SOURCE } from "./source";

export const QUERY_DEBOUNCE_MS = 200;

/** The third line of a row when the host sent no preview. */
export function rowPreview(
	row: MeetingRow,
	progress: ProgressSnapshot["entries"][number] | undefined,
): string {
	if (row.preview) {
		return row.preview;
	}
	switch (row.state) {
		case "recording":
			return "Recording now.";
		case "queued":
			return "Waiting to process.";
		case "processing":
			return progress ? `${progress.title}…` : "Processing.";
		case "failed":
			return row.failureReason
				? firstSentence(row.failureReason)
				: "Processing failed.";
		case "ready":
			return row.hasSummary ? "Summary ready." : "Transcript ready.";
	}
}

function emptyCopy(list: MeetingsListSnapshot): {
	title: string;
	body: string;
} {
	if (list.counts.all === 0) {
		return {
			title: "No meetings yet",
			body: "Press Record above, or ⌘⇧R. The menu bar item works too.",
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
 * The 300 px column on the sidebar surface: the header row and the search
 * row, then the day groups
 * with one row per meeting. The selected row is filled and stays in view.
 * Right-click or the Delete key deletes behind the host's confirmation; the
 * arrow keys move the selection.
 */
export function MeetingList() {
	const client = useBridge();
	const list = useSnapshot("meetings.list");
	const progress = useSnapshot("progress");
	const [query, setQuery] = useState("");
	const pending = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
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
			className="surface-grain flex min-h-0 flex-col overflow-hidden border-sidebar-border border-r bg-sidebar"
			data-testid="meeting-list"
			onKeyDown={onKeyDown}
		>
			<HeaderRow inset="sm">
				<h1 className="m-0 min-w-0 truncate font-medium text-foreground text-sm">
					Meetings
				</h1>
			</HeaderRow>
			<div className="flex flex-col gap-1 px-2 pb-1">
				<SearchInput
					aria-label="Search meetings"
					data-testid="search-meetings"
					onChange={onQueryChange}
					placeholder="Search meetings"
					shortcut="⌘F"
					value={query}
					variant="row"
				/>
				{list?.error ? (
					<p className="my-0 px-2 text-warning-foreground text-xs">
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
						{...emptyCopy(list)}
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

const TONE: Record<RowState, string> = {
	recording: "text-live-foreground",
	failed: "text-destructive-foreground",
	processing: "text-info-foreground",
	queued: "text-info-foreground",
};

const LABEL: Record<RowState, string> = {
	recording: "Live",
	failed: "Failed",
	processing: "Working",
	queued: "Working",
};

/**
 * Line 1's trailing slot: the start time, led by the row's state word while
 * it is not simply done.
 */
function RowStatus({ meeting }: { meeting: MeetingRow }) {
	const time = (
		<time
			className="text-muted-foreground tabular-nums"
			dateTime={meeting.startedAt}
		>
			{format.time(meeting.startedAt)}
		</time>
	);
	if (meeting.state === "ready") {
		return time;
	}
	return (
		<span className="inline-flex items-center gap-1.5">
			<span
				className={cn(
					"inline-flex items-center gap-1 font-medium",
					TONE[meeting.state],
				)}
			>
				{meeting.state === "recording" ? (
					<span
						aria-hidden="true"
						className="size-1.5 animate-status-pulse rounded-full bg-live"
					/>
				) : null}
				{LABEL[meeting.state]}
			</span>
			{time}
		</span>
	);
}

/** One meeting as T3 Code's thread card row: source and state, title, meta. */
function MeetingRowView({
	meeting,
	active,
	progress,
}: {
	meeting: MeetingRow;
	active: boolean;
	progress: ProgressSnapshot["entries"][number] | undefined;
}) {
	const client = useBridge();
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
						"relative block w-full rounded-md px-2.5 py-2 text-left outline-none transition-colors duration-(--duration-functional) ease-standard focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset",
						active ? "bg-row-selected text-foreground" : "hover:bg-row-hover",
					)}
					data-testid={`meeting-${meeting.id}`}
					onClick={() =>
						send(client, "meetings.select", { meetingID: meeting.id })
					}
					ref={row}
					type="button"
				>
					<div className="flex h-5 min-w-0 items-center gap-1.5 text-xs [&>svg]:size-4 [&>svg]:shrink-0 [&>svg]:text-sidebar-icon">
						{SOURCE[meeting.source].icon}
						<span className="min-w-0 flex-1 truncate font-medium text-muted-foreground">
							{formatSource(meeting.source)}
						</span>
						<RowStatus meeting={meeting} />
					</div>
					<div className="mt-1 truncate font-medium text-foreground text-sm">
						{meeting.title}
					</div>
					<div className="mt-0.5 flex min-w-0 items-center gap-1.5 text-muted-foreground text-xs">
						<span className="min-w-0 flex-1 truncate">
							{rowPreview(meeting, progress)}
						</span>
						<span className="shrink-0 tabular-nums">
							{format.duration(meeting.durationSeconds)}
						</span>
						{meeting.state === "ready" && !meeting.hasSummary ? (
							<Badge size="sm" variant="warning">
								No summary
							</Badge>
						) : null}
						{meeting.speakers.length > 0 ? (
							<AvatarStack className="shrink-0" ring="sidebar">
								{meeting.speakers.map((chip) => (
									<Avatar
										index={chip.colorIndex}
										initial={chip.initial}
										key={chip.id}
										name={chip.isConfirmed ? chip.initial : "Unnamed speaker"}
										size="sm"
										unknown={!chip.isConfirmed}
									/>
								))}
							</AvatarStack>
						) : null}
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
