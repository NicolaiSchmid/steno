import {
	CheckIcon,
	ChevronDownIcon,
	LoaderCircleIcon,
	UserPlusIcon,
} from "lucide-react";
import {
	type KeyboardEvent,
	type ReactNode,
	useEffect,
	useRef,
	useState,
} from "react";
import type { MeetingDetailSnapshot, SpeakerOption } from "@/bridge/contract";
import { useBridge } from "@/bridge/hooks";
import {
	Avatar,
	AvatarStack,
	Badge,
	Button,
	focusRowClass,
	Kbd,
	Popover,
	PopoverPopup,
	PopoverTrigger,
} from "@/components/ui";
import { cn } from "@/lib/cn";
import {
	type FieldAction,
	SelectFailure,
	SpeakerNameField,
	useSpeakerSelect,
} from "./speaker-name-field";
import {
	checkLabel,
	firstWithWork,
	isDismissed,
	isMe,
	isNamed,
	NOT_A_PERSON,
	needsLabel,
	nextToName,
	optionColor,
	type PanelLayout,
	type PillState,
	type Placement,
	panelLayout,
	peopleLabel,
	pillState,
	placements,
	quote,
	rowOrder,
	type Speaker,
	suggestionReason,
	unsuggested,
} from "./speaker-names";
import {
	announceAction,
	announceNamed,
	SpeakerPlayButton,
	sendAction,
	sendReject,
	togglePlay,
} from "./speaker-picker";

export interface SpeakersPopoverProps {
	speakers: MeetingDetailSnapshot["speakers"];
	transcript: MeetingDetailSnapshot["transcript"];
	/** The progress topic reports the Diarize stage for this meeting. */
	finding?: boolean;
}

const PILL_VARIANT = {
	none: "dashed",
	needs: "warning-outline",
	check: "outline",
	done: "ghost-muted",
} as const;

/** The pill's words, and the count it shows alone in a narrow header. */
function pillWords(state: PillState): [words: ReactNode, count: number] {
	switch (state.kind) {
		case "needs":
			return [needsLabel(state.count), state.count];
		case "check":
			return [checkLabel(state.count), state.count];
		case "done":
			return [peopleLabel(state.avatars), state.avatars.length];
		default:
			return ["Name speakers", 0];
	}
}

/** The header pill's inside: avatars, the words, the chevron. */
function PillContent({ state }: { state: PillState }) {
	if (state.kind === "none") {
		return (
			<>
				<UserPlusIcon aria-hidden="true" />
				Name speakers
			</>
		);
	}
	if (state.kind === "hidden" || state.kind === "finding") {
		return null;
	}
	const [words, count] = pillWords(state);
	const avatars =
		state.kind === "done" ? state.avatars.slice(0, 4) : state.avatars;
	return (
		<>
			<AvatarStack aria-hidden="true" ring="background">
				{avatars.map((speaker) => (
					<Avatar
						index={speaker.colorIndex}
						key={speaker.id}
						name={speaker.displayName}
						size="sm"
						solid
					/>
				))}
				{state.kind === "needs" ? (
					<Avatar name="Unnamed" size="sm" unknown />
				) : null}
			</AvatarStack>
			{/*
			 * In a speakers row narrower than 18 rem the words go to the
			 * screen reader and the eye gets the count, so the pill never
			 * clips "2 need names" to a sliver; it opens the same panel.
			 */}
			<span className="@max-[18rem]/speakers:sr-only truncate">{words}</span>
			<span
				aria-hidden="true"
				className="@max-[18rem]/speakers:inline hidden tabular-nums"
			>
				{count}
			</span>
			<ChevronDownIcon aria-hidden="true" />
		</>
	);
}

interface Editing {
	speakerID: string;
	text: string;
	/** "Someone else": the person the voice was suggested as. */
	excluded?: string;
}

interface Failure {
	message: string;
	/** Sends the same pick, No or action again. */
	retry: () => void;
	text: string;
}

/** A pick on its way: the name it shows, in the colour it will have. */
interface Saving {
	label: string;
	/** None for a Create: the new person's colour is the host's to fold. */
	colorIndex: number | undefined;
}

/**
 * The header's one entry point to naming: a pill whose look says whether
 * work is left, and the panel it opens. The panel lists, in this order, an
 * unnamed Me as one line, "Is this right?" (voice matches, each with the
 * clip's own words, its clip, Yes and No), "Need names" (the whole row
 * opens naming), "Speakers" (confirmed people; a person's second voice
 * nests under them, and a voice that sounds like them asks "Also
 * Nicolai?") and "Not people" (voices dismissed as music or noise, each
 * with Restore). Rows stay where they were while the panel is open; a row
 * named, dismissed or restored during the visit is drawn as it is now in
 * its old place, and a named one can be opened again to correct it, but
 * not confirmed twice. No is the exception: the suggestion goes at once,
 * the voice moves to "Need names" and its field opens without that
 * person. The field's last row is the voice's action (Not a person,
 * Remove name, Restore).
 *
 * Naming keeps the voice's evidence on screen and opens the field under
 * it. A pick shows the name with "Saving"; once the host took it the field
 * moves to the next unnamed voice, never back up and never to a
 * suggestion; a failure reopens the row with its text and a red line with
 * Retry. Combining two voices confirms first (`useSpeakerSelect`).
 *
 * Keyboard: the pill opens on the first row with work; Up and Down, or
 * Tab and Shift+Tab, move between rows, each one tab stop (Tab past the
 * last row leaves the panel); on a row Enter names, Y confirms, N says
 * No, R restores and P plays, the keys shown while keyboard focus is on
 * the row.
 * Tab and Shift+Tab leave the field for the next or previous row without a
 * pick. Escape closes the field and returns to its row; a second Escape
 * closes the panel and returns to the pill. Test ids carry a
 * `speakers-popover-` prefix so they never collide with the transcript's.
 */
export function SpeakersPopover({
	speakers,
	transcript,
	finding = false,
}: SpeakersPopoverProps) {
	const client = useBridge();
	const { select, asking, dialog } = useSpeakerSelect();
	const [open, setOpen] = useState(false);
	const [frozen, setFrozen] = useState<Map<string, Placement> | undefined>();
	const [active, setActive] = useState<string | undefined>();
	const [editing, setEditing] = useState<Editing | null>(null);
	const [saving, setSaving] = useState<ReadonlyMap<string, Saving>>(new Map());
	const [failures, setFailures] = useState<ReadonlyMap<string, Failure>>(
		new Map(),
	);
	// Voices answered No during this visit: their suggestion is gone at
	// once, before the host republishes.
	const [rejected, setRejected] = useState<ReadonlySet<string>>(new Set());
	const rows = useRef(new Map<string, HTMLElement>());
	const focusNext = useRef<string | null>(null);
	// Read after a pick's await: whether this visit is still open, and
	// which rows are saving now.
	const isOpen = useRef(false);
	const savingNow = useRef(new Set<string>());
	// The panel as last drawn: a pick that settles after another one
	// reads it, not the layout from before its await.
	const latest = useRef<PanelLayout | undefined>(undefined);

	const shown = speakers.map((speaker) =>
		rejected.has(speaker.id) && speaker.assignment === "suggested"
			? unsuggested(speaker)
			: speaker,
	);
	const state = pillState(shown, transcript.length > 0, finding);
	const layout = panelLayout(shown, frozen);
	latest.current = layout;
	const order = rowOrder(layout);
	const current =
		order.find((speaker) => speaker.id === active)?.id ?? order[0]?.id;

	useEffect(() => {
		const id = focusNext.current;
		if (id && editing?.speakerID !== id) {
			focusNext.current = null;
			// A voice combined away has no row; its place goes to the
			// current row, else the first.
			const row =
				rows.current.get(id) ??
				(current ? rows.current.get(current) : undefined) ??
				rows.current.values().next().value;
			row?.focus();
		}
	});

	useEffect(() => {
		if (editing) {
			rows.current
				.get(editing.speakerID)
				?.closest("li")
				?.scrollIntoView?.({ block: "nearest" });
		}
	}, [editing]);

	if (state.kind === "hidden") {
		return null;
	}
	if (state.kind === "finding") {
		return (
			<Button
				data-testid="speakers-finding"
				disabled
				size="pill"
				variant="outline"
			>
				<LoaderCircleIcon
					aria-hidden="true"
					className="motion-safe:animate-spin"
				/>
				Finding speakers…
			</Button>
		);
	}

	function onOpenChange(next: boolean) {
		if (!next && asking) {
			return;
		}
		setOpen(next);
		isOpen.current = next;
		setEditing(null);
		setFailures(new Map());
		setRejected(new Set());
		if (next) {
			const fresh = placements(speakers);
			setFrozen(fresh);
			const opened = panelLayout(speakers, fresh);
			setActive((firstWithWork(opened) ?? rowOrder(opened)[0])?.id);
		} else {
			setFrozen(undefined);
		}
	}

	function startNaming(speaker: Speaker, excluded?: string) {
		setActive(speaker.id);
		setEditing({
			speakerID: speaker.id,
			text: failures.get(speaker.id)?.text ?? "",
			...(excluded ? { excluded } : {}),
		});
	}

	function stopNaming(speaker: Speaker) {
		setEditing(null);
		focusNext.current = speaker.id;
	}

	/** Tab out of the field: no pick, focus on the next (or previous) row. */
	function tabFrom(speaker: Speaker, backward: boolean) {
		const at = order.findIndex((row) => row.id === speaker.id);
		const target =
			order[
				Math.min(Math.max(at + (backward ? -1 : 1), 0), order.length - 1)
			] ?? speaker;
		setEditing(null);
		setActive(target.id);
		focusNext.current = target.id;
	}

	/**
	 * Focus left the field without a pick: the edit ends. Focus stays where
	 * it went, or comes back to the row when it went nowhere; a Tab or a
	 * pick that already chose where focus goes keeps its choice.
	 */
	function leaveNaming(speaker: Speaker, to: Element | null) {
		setEditing((now) => (now?.speakerID === speaker.id ? null : now));
		if (to === null) {
			focusNext.current ??= speaker.id;
		}
	}

	/**
	 * Sends a pick and settles the row: "Saving" until the host answers,
	 * then the next unnamed voice's field, or the row again with its text
	 * (`fromField`) and, on failure, the red line with Retry.
	 */
	async function commit(
		speaker: Speaker,
		option: SpeakerOption,
		text: string,
		fromField = true,
	) {
		const work = speaker.assignment === "unknown";
		const excluded =
			editing?.speakerID === speaker.id ? editing.excluded : undefined;
		savingNow.current.add(speaker.id);
		setFailures((map) => {
			const next = new Map(map);
			next.delete(speaker.id);
			return next;
		});
		setSaving((map) =>
			new Map(map).set(speaker.id, {
				label: option.label,
				colorIndex: optionColor(option, speakers),
			}),
		);
		setEditing((now) => (now?.speakerID === speaker.id ? null : now));
		focusNext.current = speaker.id;
		const outcome = await select(speaker, option);
		savingNow.current.delete(speaker.id);
		setSaving((map) => {
			const next = new Map(map);
			next.delete(speaker.id);
			return next;
		});
		if (outcome.kind === "committed") {
			announceNamed(speaker, option.label);
		}
		if (!isOpen.current) {
			// The panel closed while saving: the next visit starts clean.
			return;
		}
		if (outcome.kind === "committed") {
			const next =
				work && latest.current
					? nextToName(latest.current, speaker.id, savingNow.current)
					: undefined;
			if (next) {
				focusNext.current = null;
				setActive(next.id);
				setEditing((now) => now ?? { speakerID: next.id, text: "" });
			}
			return;
		}
		if (outcome.kind === "failed") {
			const failure = {
				message: outcome.message,
				retry: () =>
					void commit(speaker, option, text, editing?.speakerID === speaker.id),
				text,
			};
			setFailures((map) => new Map(map).set(speaker.id, failure));
		}
		setActive(speaker.id);
		if (fromField) {
			// Failed or cancelled: the row reopens with what was typed.
			focusNext.current = null;
			setEditing({
				speakerID: speaker.id,
				text,
				...(excluded ? { excluded } : {}),
			});
		}
	}

	function confirm(speaker: Speaker) {
		// Confirmed already, or on its way: a second Yes sends nothing.
		if (
			!speaker.personID ||
			isNamed(speaker) ||
			savingNow.current.has(speaker.id)
		) {
			return;
		}
		void commit(
			speaker,
			{
				kind: "person",
				label: speaker.suggestionName ?? speaker.displayName,
				personID: speaker.personID,
			},
			"",
			false,
		);
	}

	/** Shows "Saving" on the row, or clears it. */
	function markSaving(speaker: Speaker, label: string | null) {
		if (label === null) {
			savingNow.current.delete(speaker.id);
		} else {
			savingNow.current.add(speaker.id);
		}
		setSaving((map) => {
			const next = new Map(map);
			if (label === null) {
				next.delete(speaker.id);
			} else {
				next.set(speaker.id, { label, colorIndex: speaker.colorIndex });
			}
			return next;
		});
	}

	function fail(speaker: Speaker, message: string, retry: () => void) {
		setFailures((map) =>
			new Map(map).set(speaker.id, { message, retry, text: "" }),
		);
	}

	function clearFailure(speaker: Speaker) {
		setFailures((map) => {
			const next = new Map(map);
			next.delete(speaker.id);
			return next;
		});
	}

	/**
	 * No to "Sounds like Anna": the suggestion goes at once, the voice moves
	 * to "Need names" and its field opens without Anna, while the host
	 * records the No (`speakers.reject`). On failure the suggestion comes
	 * back with the red line and Retry.
	 */
	async function reject(speaker: Speaker) {
		const personID = speaker.personID;
		if (!personID || isNamed(speaker) || savingNow.current.has(speaker.id)) {
			return;
		}
		const before = frozen?.get(speaker.id);
		clearFailure(speaker);
		setRejected((set) => new Set(set).add(speaker.id));
		setFrozen(
			(map) => map && new Map(map).set(speaker.id, { section: "need" }),
		);
		startNaming(speaker, personID);
		const failure = await sendReject(client, speaker.id, personID);
		if (failure === null || !isOpen.current) {
			return;
		}
		setRejected((set) => {
			const next = new Set(set);
			next.delete(speaker.id);
			return next;
		});
		setFrozen(
			(map) =>
				map && (before ? new Map(map).set(speaker.id, before) : new Map(map)),
		);
		setEditing((now) => (now?.speakerID === speaker.id ? null : now));
		setActive(speaker.id);
		focusNext.current = speaker.id;
		fail(speaker, failure, () => void reject(speaker));
	}

	/**
	 * Not a person, Remove name or Restore: "Saving" on the row, which keeps
	 * its place and is drawn as the host republishes it; the result is
	 * announced, a failure is the red line with Retry.
	 */
	async function act(speaker: Speaker, action: FieldAction) {
		if (savingNow.current.has(speaker.id)) {
			return;
		}
		clearFailure(speaker);
		setEditing((now) => (now?.speakerID === speaker.id ? null : now));
		setActive(speaker.id);
		focusNext.current = speaker.id;
		markSaving(
			speaker,
			action === "dismiss" ? NOT_A_PERSON : speaker.clusterLabel,
		);
		const failure = await sendAction(client, speaker.id, action);
		markSaving(speaker, null);
		if (failure === null) {
			announceAction(speaker, action);
		} else if (isOpen.current) {
			fail(speaker, failure, () => void act(speaker, action));
		}
	}

	function onRowKey(
		event: KeyboardEvent<HTMLElement>,
		speaker: Speaker,
		kind: RowKind,
	) {
		if (
			event.target !== event.currentTarget ||
			event.metaKey ||
			event.ctrlKey ||
			event.altKey
		) {
			return;
		}
		// A row saving takes no new pick; a named row is not confirmed again.
		const inert = saving.has(speaker.id);
		const at = order.findIndex((row) => row.id === speaker.id);
		const move: Record<string, number> = {
			ArrowDown: Math.min(at + 1, order.length - 1),
			ArrowUp: Math.max(at - 1, 0),
			Home: 0,
			End: order.length - 1,
		};
		const key = event.key.length === 1 ? event.key.toLowerCase() : event.key;
		// Tab walks the rows too, so each one is reached without the
		// arrows; past the last (or before the first) it leaves the panel.
		const tabTo =
			key === "Tab" ? order[at + (event.shiftKey ? -1 : 1)] : undefined;
		if (tabTo) {
			event.preventDefault();
			setActive(tabTo.id);
			rows.current.get(tabTo.id)?.focus();
		} else if (key in move) {
			event.preventDefault();
			const target = order[move[key] ?? at];
			if (target) {
				setActive(target.id);
				rows.current.get(target.id)?.focus();
			}
		} else if (inert && key !== "p") {
			return;
		} else if (kind === "dismissed" && (key === "Enter" || key === "r")) {
			event.preventDefault();
			void act(speaker, "restore");
		} else if (kind === "dismissed") {
			if (key === "p") {
				event.preventDefault();
				togglePlay(client, speaker);
			}
		} else if (key === "Enter" || key === " ") {
			event.preventDefault();
			startNaming(speaker);
		} else if (
			key === "y" &&
			(kind === "check" || kind === "also") &&
			!isNamed(speaker)
		) {
			event.preventDefault();
			confirm(speaker);
		} else if (
			key === "n" &&
			(kind === "check" || kind === "also") &&
			!isNamed(speaker)
		) {
			event.preventDefault();
			void reject(speaker);
		} else if (key === "p") {
			event.preventDefault();
			togglePlay(client, speaker);
		}
	}

	const row = (speaker: Speaker, placed: RowKind, person?: Speaker) => {
		// Drawn as it is now: dismissed during the visit in its old place,
		// or restored while still listed under "Not people".
		const kind: RowKind = isDismissed(speaker)
			? "dismissed"
			: placed === "dismissed"
				? "need"
				: placed;
		return (
			<PanelRow
				editing={editing?.speakerID === speaker.id ? editing : null}
				failure={failures.get(speaker.id)}
				key={speaker.id}
				kind={kind}
				onAction={(action) => void act(speaker, action)}
				onCancel={() => stopNaming(speaker)}
				onConfirm={() => confirm(speaker)}
				onFocus={() => setActive(speaker.id)}
				onKeyDown={(event) => onRowKey(event, speaker, kind)}
				onLeave={(to) => leaveNaming(speaker, to)}
				onName={() => startNaming(speaker)}
				onReject={() => void reject(speaker)}
				onRetry={(failure) => failure.retry()}
				onSubmit={(option, text) => void commit(speaker, option, text)}
				onTab={(backward) => tabFrom(speaker, backward)}
				person={person}
				rowRef={(element) => {
					if (element) {
						rows.current.set(speaker.id, element);
					} else {
						rows.current.delete(speaker.id);
					}
				}}
				saving={saving.get(speaker.id)}
				speaker={speaker}
				speakers={shown}
				tabIndex={speaker.id === current ? 0 : -1}
			/>
		);
	};

	return (
		<Popover modal={false} onOpenChange={onOpenChange} open={open}>
			<PopoverTrigger
				className="min-w-0 max-w-full shrink"
				data-testid="speakers-trigger"
				render={<Button size="pill" variant={PILL_VARIANT[state.kind]} />}
			>
				<PillContent state={state} />
			</PopoverTrigger>
			<PopoverPopup
				align="start"
				aria-label="Speakers"
				// Always under the pill: in a short or zoomed window it takes
				// the height left there and scrolls, the pill still in view.
				collisionAvoidance={{ side: "none", fallbackAxisSide: "none" }}
				data-testid="speakers-panel"
				initialFocus={() =>
					(current ? rows.current.get(current) : undefined) ?? true
				}
				padding="panel"
				size="panel"
			>
				<Sections layout={layout} row={row} />
				{dialog}
			</PopoverPopup>
		</Popover>
	);
}

function Sections({
	layout,
	row,
}: {
	layout: PanelLayout;
	row: (speaker: Speaker, kind: RowKind, person?: Speaker) => ReactNode;
}) {
	return (
		<>
			{layout.me ? (
				<ul className="m-0 list-none p-0">{row(layout.me, "me")}</ul>
			) : null}
			<Section
				// Every match answered during the visit: the heading says so.
				label={
					layout.check.length > 0 && layout.check.every(isNamed)
						? "Checked"
						: "Is this right?"
				}
				testID="speakers-section-check"
			>
				{layout.check.map((speaker) => row(speaker, "check"))}
			</Section>
			<Section label="Need names" testID="speakers-section-need">
				{layout.need.map((speaker) => row(speaker, "need"))}
			</Section>
			<Section label="Speakers" testID="speakers-section-people">
				{layout.people.map(({ speaker, voices, also }) => {
					const nested = voices.length > 0 || also.length > 0;
					if (!nested) {
						return row(speaker, "person");
					}
					return (
						<li className="flex flex-col" key={speaker.id}>
							<ul className="m-0 list-none p-0">{row(speaker, "person")}</ul>
							<ul
								className="m-0 mb-1 ml-[19px] flex list-none flex-col gap-0.5 border-border border-l p-0 pl-2"
								data-testid={`speakers-nested-${speaker.id}`}
							>
								{voices.map((voice) => row(voice, "voice", speaker))}
								{also.map((voice) => row(voice, "also", speaker))}
							</ul>
						</li>
					);
				})}
			</Section>
			<Section label="Not people" testID="speakers-section-dismissed">
				{layout.dismissed.map((speaker) => row(speaker, "dismissed"))}
			</Section>
		</>
	);
}

function Section({
	label,
	testID,
	children,
}: {
	label: string;
	testID: string;
	children: ReactNode[];
}) {
	if (children.length === 0) {
		return null;
	}
	return (
		<section className="mt-2 first:mt-0" data-testid={testID}>
			<h3 className="m-0 px-2 pt-1 pb-1 font-medium text-meta text-muted-foreground">
				{label}
			</h3>
			<ul className="m-0 flex list-none flex-col gap-0.5 p-0">{children}</ul>
		</section>
	);
}

type RowKind =
	| "me"
	| "check"
	| "need"
	| "person"
	| "voice"
	| "also"
	| "dismissed";

interface PanelRowProps {
	speaker: Speaker;
	kind: RowKind;
	/** The person a nested voice belongs to, or sounds like. */
	person: Speaker | undefined;
	speakers: readonly Speaker[];
	editing: Editing | null;
	saving: Saving | undefined;
	failure: Failure | undefined;
	tabIndex: number;
	rowRef: (element: HTMLElement | null) => void;
	onFocus: () => void;
	onKeyDown: (event: KeyboardEvent<HTMLElement>) => void;
	onName: () => void;
	onConfirm: () => void;
	/** No to the suggestion (or "Someone else"). */
	onReject: () => void;
	/** Not a person, Remove name or Restore. */
	onAction: (action: FieldAction) => void;
	onSubmit: (option: SpeakerOption, text: string) => void;
	onCancel: () => void;
	onLeave: (to: Element | null) => void;
	onTab: (backward: boolean) => void;
	onRetry: (failure: Failure) => void;
}

/** What a row says for itself to a screen reader, and which keys it takes. */
function rowLabel(speaker: Speaker, kind: RowKind, person?: Speaker): string {
	if (isNamed(speaker) && kind !== "person" && kind !== "me") {
		// Named during this visit, still in its section.
		return `${speaker.clusterLabel}, ${speaker.displayName}`;
	}
	switch (kind) {
		case "me":
			return isNamed(speaker)
				? `You, ${speaker.displayName}`
				: "You spoke on the mic. Name yourself";
		case "check":
			return `${speaker.clusterLabel}. ${suggestionReason(speaker)}`;
		case "need":
			return `${speaker.clusterLabel}, needs a name`;
		case "also":
			return `${speaker.clusterLabel}. Also ${person?.displayName ?? speaker.displayName}?`;
		case "dismissed":
			return `${speaker.clusterLabel}, not a person`;
		case "voice":
			return `${speaker.clusterLabel}, ${speaker.displayName}`;
		default:
			return isMe(speaker)
				? `${speaker.displayName}, you`
				: speaker.displayName;
	}
}

const KEYS: Record<RowKind, string> = {
	me: "Enter",
	check: "Enter Y N P",
	need: "Enter P",
	person: "Enter P",
	voice: "Enter P",
	also: "Enter Y N P",
	dismissed: "Enter R P",
};

/** A row's key, shown only while keyboard focus is on the row. */
function KeyHint({ children }: { children: string }) {
	return (
		<Kbd
			aria-hidden="true"
			// 6 px from the button's word: its own 4 px gap and 2 more.
			className="ml-0.5 hidden group-focus-visible/row:inline-flex"
			variant="cap"
		>
			{children}
		</Kbd>
	);
}

/**
 * One row of the panel: one tab stop (its buttons are reached by the
 * row's keys, hinted while the row has keyboard focus, or the pointer),
 * with the voice's evidence (the clip's own words, its clip) on screen
 * while it is named, the field under them. A row named during the visit
 * is done: it opens again to correct the name, and offers no second Yes.
 * A voice that is not a person reads so, with Restore.
 */
function PanelRow({
	speaker,
	kind,
	person,
	speakers,
	editing,
	saving,
	failure,
	tabIndex,
	rowRef,
	onFocus,
	onKeyDown,
	onName,
	onConfirm,
	onReject,
	onAction,
	onSubmit,
	onCancel,
	onLeave,
	onTab,
	onRetry,
}: PanelRowProps) {
	const named = isNamed(speaker);
	// Settled during this visit: confirmed, but still listed with the work.
	const done =
		named &&
		(kind === "me" || kind === "check" || kind === "need" || kind === "also");
	// A confirmed person's line shows while they are renamed, as evidence.
	const said =
		(kind === "person" || kind === "me") && !editing ? "" : quote(speaker);
	// The whole row opens naming, except where Yes and No, or Restore, sit.
	const opens =
		(done || (kind !== "check" && kind !== "also" && kind !== "dismissed")) &&
		!editing &&
		saving === undefined;

	const title: ReactNode =
		saving !== undefined ? (
			<>
				<span className="min-w-0 truncate font-medium text-title">
					{saving.label}
				</span>
				<span
					className="shrink-0 text-meta text-muted-foreground"
					data-testid={`speakers-popover-saving-${speaker.id}`}
				>
					Saving
				</span>
			</>
		) : done ? (
			<>
				<span className="min-w-0 truncate font-medium text-title">
					{kind === "me"
						? `You are ${speaker.displayName}`
						: speaker.displayName}
				</span>
				<CheckIcon
					aria-label="Named"
					className="size-3.5 shrink-0 text-muted-foreground"
				/>
			</>
		) : kind === "me" ? (
			<>
				<span className="min-w-0 truncate text-body text-muted-foreground">
					You spoke on the mic.
				</span>
				<Badge size="chip" variant="dashed">
					Name yourself
				</Badge>
			</>
		) : kind === "need" ? (
			<Badge className="min-w-0" size="chip" variant="dashed">
				<span className="truncate">{speaker.clusterLabel}</span>
			</Badge>
		) : kind === "dismissed" ? (
			<>
				<span className="shrink-0 text-title">{speaker.clusterLabel}</span>
				<span className="min-w-0 truncate text-meta text-muted-foreground">
					{NOT_A_PERSON}
				</span>
			</>
		) : kind === "check" ? (
			<>
				<span className="shrink-0 font-medium text-title">
					{speaker.clusterLabel}
				</span>
				<span className="min-w-0 truncate text-meta text-muted-foreground">
					{suggestionReason(speaker)}
				</span>
			</>
		) : kind === "also" ? (
			<>
				<span className="min-w-0 truncate font-medium text-title">
					Also {person?.displayName}?
				</span>
				<span className="shrink-0 text-meta text-muted-foreground">
					{speaker.clusterLabel}
				</span>
			</>
		) : kind === "voice" ? (
			<span className="min-w-0 truncate text-title">
				{speaker.clusterLabel}
			</span>
		) : (
			<>
				<span
					className="min-w-0 truncate font-medium text-title"
					title={speaker.displayName}
				>
					{speaker.displayName}
				</span>
				{isMe(speaker) ? (
					<span className="shrink-0 text-meta text-muted-foreground">You</span>
				) : null}
			</>
		);

	const avatar =
		kind === "voice" || kind === "also" || kind === "me" ? null : (
			<Avatar
				aria-hidden="true"
				index={saving ? (saving.colorIndex ?? 0) : speaker.colorIndex}
				name={saving?.label ?? speaker.displayName}
				size="lg"
				unknown={
					saving
						? saving.colorIndex === undefined
						: speaker.assignment === "unknown"
				}
			/>
		);
	const failureLine = failure ? (
		<SelectFailure
			className="mt-1.5"
			message={failure.message}
			onRetry={() => onRetry(failure)}
			testID={`speakers-popover-failure-${speaker.id}`}
		/>
	) : null;
	const actions =
		kind === "dismissed" && saving === undefined ? (
			<div className="mt-1.5 flex gap-2">
				<Button
					data-testid={`speakers-popover-restore-${speaker.id}`}
					onClick={(event) => {
						event.stopPropagation();
						onAction("restore");
					}}
					size="xs"
					tabIndex={-1}
					variant="outline"
				>
					Restore
					<KeyHint>R</KeyHint>
				</Button>
			</div>
		) : (kind === "check" || kind === "also") &&
			!done &&
			saving === undefined &&
			!editing ? (
			<div className="mt-1.5 flex gap-2">
				<Button
					data-testid={`speakers-popover-yes-${speaker.id}`}
					onClick={(event) => {
						event.stopPropagation();
						onConfirm();
					}}
					size="xs"
					tabIndex={-1}
					variant="outline"
				>
					{kind === "also" ? "Same person" : "Yes"}
					<KeyHint>Y</KeyHint>
				</Button>
				<Button
					data-testid={`speakers-popover-no-${speaker.id}`}
					onClick={(event) => {
						event.stopPropagation();
						onReject();
					}}
					size="xs"
					tabIndex={-1}
					variant="ghost"
				>
					{kind === "also" ? "Someone else" : "No"}
					<KeyHint>N</KeyHint>
				</Button>
			</div>
		) : null;

	return (
		<li
			className="flex flex-col"
			data-naming-scope=""
			data-testid={`speakers-popover-row-${speaker.id}`}
		>
			{/* biome-ignore lint/a11y/useSemanticElements: a row of the panel holds its own buttons, so it cannot be one; it is one tab stop with keys. */}
			<div
				aria-keyshortcuts={done ? "Enter P" : KEYS[kind]}
				aria-label={rowLabel(speaker, kind, person)}
				className={cn(
					"group/row flex min-w-0 gap-2.5 rounded-md px-2 py-1.5",
					focusRowClass,
					opens && "cursor-default hover:bg-row-hover",
				)}
				data-testid={`speakers-popover-name-${speaker.id}`}
				onClick={opens ? () => onName() : undefined}
				onFocus={onFocus}
				onKeyDown={onKeyDown}
				ref={rowRef}
				role="group"
				tabIndex={tabIndex}
			>
				{avatar}
				<div className="flex min-w-0 flex-1 flex-col">
					<div className="flex min-h-8 min-w-0 items-center gap-2">
						<div className="flex min-w-0 flex-1 items-center gap-2">
							{title}
						</div>
						{kind === "me" || (kind === "person" && isMe(speaker)) ? null : (
							<SpeakerPlayButton
								keyHint
								speaker={speaker}
								tabIndex={-1}
								testID={`speakers-popover-play-${speaker.id}`}
							/>
						)}
					</div>
					{said ? (
						<p
							className="m-0 line-clamp-2 text-meta text-muted-foreground"
							data-testid={`speakers-popover-snippet-${speaker.id}`}
						>
							“{said}”
						</p>
					) : null}
					{actions}
				</div>
			</div>
			{editing ? (
				<SpeakerNameField
					className={cn("mt-1 mb-1.5 px-2", avatar && "pl-[50px]")}
					{...(editing.excluded ? { excludedPersonID: editing.excluded } : {})}
					initialText={editing.text}
					listClassName="max-h-40"
					notice={failureLine}
					onAction={onAction}
					onCancel={onCancel}
					onLeave={onLeave}
					onSubmit={onSubmit}
					onTab={onTab}
					speaker={speaker}
					speakers={speakers}
				/>
			) : failureLine ? (
				<div className={cn("mb-1.5 px-2", avatar && "pl-[50px]")}>
					{failureLine}
				</div>
			) : null}
		</li>
	);
}
