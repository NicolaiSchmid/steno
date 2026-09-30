import { AudioWaveformIcon, SearchIcon, Trash2Icon } from "lucide-react";
import {
	type ChangeEvent,
	Fragment,
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
	ScrollArea,
	SearchInput,
} from "@/components/ui";
import { cn } from "@/lib/cn";
import { deleteMeeting } from "./delete-meeting";
import { firstSentence, format, formatSource } from "./format";

export const QUERY_DEBOUNCE_MS = 200;

/** The second line of a row when the host sent no preview. */
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

function deleteQuietly(client: ReturnType<typeof useBridge>, id: string) {
	deleteMeeting(client, id).catch((cause: unknown) => {
		console.error("bridge: delete failed", cause);
	});
}

/**
 * The 320 pt column: the heading and search, then the day groups with one
 * row per meeting. The selected row is a raised card and stays in view.
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
				deleteQuietly(client, list.selection);
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
			className="flex min-h-0 flex-col overflow-hidden border-border border-r"
			data-testid="meeting-list"
			onKeyDown={onKeyDown}
		>
			<header className="flex flex-col gap-2.5 px-3 pt-[52px] pb-2">
				<h1 className="mx-1 my-0 font-semibold text-[15px]">Meetings</h1>
				<SearchInput
					aria-label="Search meetings"
					data-testid="search-meetings"
					onChange={onQueryChange}
					placeholder="Search meetings"
					shortcut="⌘F"
					value={query}
				/>
				{list?.error ? (
					<p className="mx-1 my-0 text-warning text-xs">{list.error}</p>
				) : null}
			</header>
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
					return (
						<Fragment key={group.day}>
							<div className="px-4 pt-3.5 pb-1.5 font-medium text-[11px] text-faint">
								{day.label}
								<span className="ml-1.5 font-normal text-muted-foreground">
									{day.date}
								</span>
							</div>
							{group.meetings.map((meeting) => (
								<MeetingRowView
									active={meeting.id === list.selection}
									key={meeting.id}
									meeting={meeting}
									progress={entries.get(meeting.id)}
								/>
							))}
						</Fragment>
					);
				})}
			</ScrollArea>
		</section>
	);
}

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
			<ContextMenuTrigger className="mx-2 mb-1 block">
				<button
					aria-current={active ? "true" : undefined}
					className={cn(
						"block w-full rounded-lg border px-[11px] pt-[9px] pb-2.5 text-left outline-none transition-colors duration-(--duration-functional) ease-standard focus-visible:ring-2 focus-visible:ring-primary/50",
						active
							? "border-border bg-card shadow-xs"
							: "border-transparent hover:bg-accent",
					)}
					data-testid={`meeting-${meeting.id}`}
					onClick={() =>
						send(client, "meetings.select", { meetingID: meeting.id })
					}
					ref={row}
					type="button"
				>
					<div className="flex items-baseline gap-2.5 font-medium text-[13px]">
						<span className="min-w-0 flex-1 truncate">{meeting.title}</span>
						<time
							className="font-mono text-[11px] text-faint tabular-nums"
							dateTime={meeting.startedAt}
						>
							{format.time(meeting.startedAt)}
						</time>
					</div>
					<p className="my-0 mt-[3px] line-clamp-2 text-muted-foreground text-xs leading-[1.45]">
						{rowPreview(meeting, progress)}
					</p>
					<div className="mt-2 flex items-center gap-2 text-[11px] text-faint">
						<Badge>{formatSource(meeting.source)}</Badge>
						<span className="font-mono tabular-nums">
							{format.duration(meeting.durationSeconds)}
						</span>
						{meeting.state === "failed" ? (
							<Badge variant="warn">Failed</Badge>
						) : meeting.state === "ready" && !meeting.hasSummary ? (
							<Badge variant="warn">No summary</Badge>
						) : meeting.state === "recording" ? (
							<Badge variant="live">Live</Badge>
						) : null}
						{meeting.speakers.length > 0 ? (
							<AvatarStack ring={active ? "card" : "background"}>
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
					onClick={() => deleteQuietly(client, meeting.id)}
					variant="destructive"
				>
					Delete meeting…
				</ContextMenuItem>
			</ContextMenuPopup>
		</ContextMenu>
	);
}
