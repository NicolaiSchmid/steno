import { PlusIcon } from "lucide-react";
import {
	type ChangeEvent,
	type FocusEvent,
	type KeyboardEvent,
	type ReactNode,
	useEffect,
	useId,
	useRef,
	useState,
} from "react";
import type { BridgeClient } from "@/bridge/client";
import type { SpeakerOption } from "@/bridge/contract";
import { useBridge } from "@/bridge/hooks";
import { BridgeError } from "@/bridge/transport";
import {
	Avatar,
	Button,
	ConfirmDialog,
	Input,
	ResultLine,
} from "@/components/ui";
import { menuItemVariants } from "@/components/ui/menu";
import { cn } from "@/lib/cn";
import {
	groupOptions,
	isMe,
	type OptionRow,
	optionText,
	type Speaker,
	voiceLabel,
} from "./speaker-names";

const SPEAKER_QUERY_DEBOUNCE_MS = 150;

/**
 * How a pick ended: the host took it, the user cancelled the combine, or it
 * failed with the host's message.
 */
export type SelectOutcome =
	| { kind: "committed" }
	| { kind: "cancelled" }
	| { kind: "failed"; message: string };

/** Asks before combining two voices; resolves whether to go on. */
export type ConfirmCombine = (ask: {
	title: string;
	consequence: string;
}) => Promise<boolean>;

/**
 * Sends a pick as `speakers.select` without `combine`. When the host
 * refuses it as `wouldCombine` (the person already owns another voice of
 * the meeting, so naming would merge the two), `confirm` asks first, with
 * the host's words and what it costs, and the pick goes again with
 * `combine: true`; Cancel leaves it. Whether a pick combines is the host's
 * call, so a Create or a Suggested pick that resolves to an owner asks too.
 * Never rejects: any failure comes back as `failed`.
 */
export async function selectSpeaker(
	client: BridgeClient,
	speaker: Speaker,
	option: SpeakerOption,
	confirm: ConfirmCombine,
): Promise<SelectOutcome> {
	const params = { speakerID: speaker.id, option };
	try {
		try {
			await client.call("speakers.select", { ...params, combine: false });
		} catch (cause) {
			if (!(cause instanceof BridgeError) || cause.code !== "wouldCombine") {
				throw cause;
			}
			const confirmed = await confirm({
				title: `Combine ${speaker.clusterLabel} with ${option.label}?`,
				consequence: `${cause.message} Their paragraphs become ${option.label}’s. This cannot be undone.`,
			});
			if (!confirmed) {
				return { kind: "cancelled" };
			}
			await client.call("speakers.select", { ...params, combine: true });
		}
		return { kind: "committed" };
	} catch (cause) {
		console.error("bridge: speakers.select failed", cause);
		return {
			kind: "failed",
			message: cause instanceof Error ? cause.message : String(cause),
		};
	}
}

/**
 * `selectSpeaker` with its combine confirm: `select` sends a pick, and
 * `dialog`, rendered once by the caller, is the confirm it may open (the
 * consequence in words, Cancel first, a red Combine).
 */
export function useSpeakerSelect(): {
	select: (speaker: Speaker, option: SpeakerOption) => Promise<SelectOutcome>;
	asking: boolean;
	dialog: ReactNode;
} {
	const client = useBridge();
	const [ask, setAsk] = useState<{
		title: string;
		consequence: string;
		resolve: (confirmed: boolean) => void;
	} | null>(null);

	const confirm: ConfirmCombine = (question) =>
		new Promise((resolve) => setAsk({ ...question, resolve }));

	function close(confirmed: boolean) {
		ask?.resolve(confirmed);
		setAsk(null);
	}

	return {
		select: (speaker, option) =>
			selectSpeaker(client, speaker, option, confirm),
		asking: ask !== null,
		dialog: (
			<ConfirmDialog
				confirmLabel="Combine"
				confirmTestId="speakers-combine"
				consequence={ask?.consequence ?? ""}
				onConfirm={() => close(true)}
				onOpenChange={(open) => {
					if (!open && ask) {
						close(false);
					}
				}}
				open={ask !== null}
				title={ask?.title ?? ""}
			/>
		),
	};
}

/**
 * A failed pick where it happened: the host's message in red, announced,
 * with Retry, which sends the same pick again.
 */
export function SelectFailure({
	message,
	onRetry,
	testID,
	className,
}: {
	message: string;
	onRetry: () => void;
	testID?: string;
	className?: string;
}) {
	return (
		<ResultLine
			actions={
				<Button
					data-testid={testID ? `${testID}-retry` : undefined}
					onClick={onRetry}
					size="xs"
					variant="outline"
				>
					Retry
				</Button>
			}
			className={className}
			data-testid={testID}
			tone="destructive"
		>
			{message}
		</ResultLine>
	);
}

export interface SpeakerNameFieldProps {
	speaker: Speaker;
	/** Every speaker of the meeting, for "Also speaking here". */
	speakers: readonly Speaker[];
	/** Called with the highlighted option and the typed text on Enter or a click. */
	onSubmit: (option: SpeakerOption, text: string) => void;
	/** Escape, and with `cancelOnBlur` focus leaving the field and its options. */
	onCancel: () => void;
	/** The text to reopen with after a failed or cancelled pick. */
	initialText?: string;
	/** A person not to offer ("Someone else" after "Also Nicolai?"). */
	excludedPersonID?: string;
	cancelOnBlur?: boolean;
	/** Under the field, above the options: a failed pick's line with Retry. */
	notice?: ReactNode;
	/** The options list's height cap; the panel keeps the evidence above it. */
	listClassName?: string;
	className?: string;
}

type Answer =
	| { query: string; kind: "options"; options: readonly SpeakerOption[] }
	| { query: string; kind: "failed" };

/**
 * The text field that names one voice, with the host's options under it
 * (`groupOptions`), asked for as the text changes (`speakers.options`,
 * debounced). Up and Down move the highlight; Home and End stay with the
 * caret. Enter acts only on the highlighted row, and only once the options
 * are for the text on screen: while they load the list says "Searching…"
 * and Enter does nothing, so nothing is chosen that was not seen. If they
 * fail to load the list says so and keeps a highlighted Create row for the
 * typed name. Escape gives up; an input method's composing Enter is left
 * alone. With `cancelOnBlur`, focus leaving the field ends the edit, unless
 * it stays inside the nearest `data-naming-scope` (the row's evidence).
 * The placeholder is the voice's label; nothing is prefilled.
 */
export function SpeakerNameField({
	speaker,
	speakers,
	onSubmit,
	onCancel,
	initialText = "",
	excludedPersonID,
	cancelOnBlur = false,
	notice,
	listClassName,
	className,
}: SpeakerNameFieldProps) {
	const client = useBridge();
	const id = useId();
	const [query, setQuery] = useState(initialText);
	const [answer, setAnswer] = useState<Answer | null>(null);
	const [active, setActive] = useState(0);
	const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
	const request = useRef(0);
	const list = useRef<HTMLDivElement>(null);

	function load(text: string) {
		const ticket = ++request.current;
		client
			.call("speakers.options", { speakerID: speaker.id, query: text })
			.then((reply) => {
				if (ticket === request.current) {
					setAnswer({ query: text, kind: "options", options: reply.options });
					setActive(0);
				}
			})
			.catch((cause: unknown) => {
				console.error("bridge: speakers.options failed", cause);
				if (ticket === request.current) {
					setAnswer({ query: text, kind: "failed" });
					setActive(0);
				}
			});
	}

	// biome-ignore lint/correctness/useExhaustiveDependencies: one first request per speaker; `load` reads the latest id through the closure.
	useEffect(() => {
		load(initialText);
		return () => {
			clearTimeout(timer.current);
			request.current += 1;
		};
	}, [speaker.id]);

	const typed = query.trim();
	const loading = answer === null || answer.query !== query;
	const failed = !loading && answer.kind === "failed";
	const groups = loading
		? []
		: answer.kind === "options"
			? groupOptions(answer.options, query, speaker, speakers, excludedPersonID)
			: typed
				? [
						{
							id: "create" as const,
							label: "New person",
							rows: [
								{
									option: { kind: "create" as const, label: typed },
									colorIndex: 0,
								},
							],
						},
					]
				: [];
	const flat: OptionRow[] = groups.flatMap((group) => group.rows);
	const highlightedAt = Math.min(active, flat.length - 1);
	const highlighted = flat[highlightedAt];

	// biome-ignore lint/correctness/useExhaustiveDependencies: follows the highlight only.
	useEffect(() => {
		list.current
			?.querySelector("[data-highlighted]")
			?.scrollIntoView?.({ block: "nearest" });
	}, [highlightedAt, answer]);

	function onChange(event: ChangeEvent<HTMLInputElement>) {
		const value = event.target.value;
		setQuery(value);
		clearTimeout(timer.current);
		timer.current = setTimeout(() => load(value), SPEAKER_QUERY_DEBOUNCE_MS);
	}

	function onKeyDown(event: KeyboardEvent<HTMLInputElement>) {
		// WebKit sends the Enter that ends a composition as keyCode 229.
		if (event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) {
			return;
		}
		if (event.key === "ArrowDown" || event.key === "ArrowUp") {
			event.preventDefault();
			if (flat.length > 0) {
				const step = event.key === "ArrowDown" ? 1 : -1;
				setActive(Math.min(Math.max(highlightedAt + step, 0), flat.length - 1));
			}
		} else if (event.key === "Enter") {
			event.preventDefault();
			if (!loading && highlighted) {
				onSubmit(highlighted.option, query);
			}
		} else if (event.key === "Escape") {
			// The field gives up; the popover around it stays open.
			event.preventDefault();
			event.stopPropagation();
			event.nativeEvent.stopImmediatePropagation();
			onCancel();
		}
	}

	function onBlur(event: FocusEvent<HTMLDivElement>) {
		// Focus may go to the evidence around the field (the row's play
		// button) without ending the edit: `data-naming-scope` marks it.
		const scope =
			event.currentTarget.closest("[data-naming-scope]") ?? event.currentTarget;
		if (cancelOnBlur && !scope.contains(event.relatedTarget)) {
			onCancel();
		}
	}

	const listID = `${id}-options`;
	const optionID = (index: number) => `${id}-option-${index}`;

	return (
		// biome-ignore lint/a11y/noStaticElementInteractions: focus leaving the field and its options, together, ends editing.
		<div className={className} onBlur={onBlur}>
			<Input
				aria-activedescendant={
					highlighted && !loading ? optionID(highlightedAt) : undefined
				}
				aria-autocomplete="list"
				aria-busy={loading}
				aria-controls={listID}
				aria-expanded={flat.length > 0}
				aria-label={`Name for ${speaker.clusterLabel}`}
				autoFocus
				data-testid={`speaker-field-${speaker.id}`}
				onChange={onChange}
				onKeyDown={onKeyDown}
				placeholder={isMe(speaker) ? "Your name" : voiceLabel(speaker)}
				role="combobox"
				size="sm"
				value={query}
			/>
			{notice}
			{loading ? (
				<p
					className="m-0 mt-1.5 px-2 py-1 text-body text-muted-foreground"
					data-testid="speaker-options-searching"
				>
					Searching…
				</p>
			) : failed ? (
				<ResultLine
					className="mt-1.5"
					data-testid="speaker-options-failed"
					tone="destructive"
				>
					People could not be loaded
				</ResultLine>
			) : null}
			<div
				aria-label="People"
				className={cn(
					"mt-1.5 flex flex-col gap-0.5 overflow-y-auto empty:hidden",
					listClassName ?? "max-h-64",
				)}
				id={listID}
				// Clicks in the list keep focus in the field, so the edit goes on.
				onMouseDown={(event) => event.preventDefault()}
				ref={list}
				role="listbox"
			>
				{groups.map((group) => (
					// biome-ignore lint/a11y/useSemanticElements: a listbox groups its options with role="group"; a fieldset is for form controls.
					<div
						aria-label={group.label}
						className="flex flex-col gap-0.5"
						key={group.id}
						role="group"
					>
						{group.id !== "create" ? (
							<div
								aria-hidden="true"
								className="px-2 pt-1.5 pb-0.5 text-caption text-muted-foreground"
							>
								{group.label}
							</div>
						) : null}
						{group.rows.map((row) => {
							const at = flat.indexOf(row);
							const isActive = row === highlighted;
							const { option } = row;
							return (
								// biome-ignore lint/a11y/useKeyWithClickEvents: focus stays in the combobox, which owns the keys (aria-activedescendant).
								<div
									aria-selected={isActive}
									className={menuItemVariants()}
									data-highlighted={isActive ? "" : undefined}
									id={optionID(at)}
									key={`${option.kind}-${option.personID ?? option.label}`}
									onClick={() => onSubmit(option, query)}
									onMouseMove={() => setActive(at)}
									role="option"
									tabIndex={-1}
								>
									{option.kind === "person" ? (
										<Avatar
											aria-hidden="true"
											index={row.colorIndex}
											name={option.label}
											size="sm"
										/>
									) : (
										<PlusIcon aria-hidden="true" />
									)}
									<span className="min-w-0 flex-1 truncate">
										{optionText(option)}
										{row.combines ? (
											<span className="text-muted-foreground">
												{" · "}
												{row.combines}
											</span>
										) : null}
									</span>
									{row.reason ? (
										<span className="shrink-0 text-meta text-muted-foreground">
											{row.reason}
										</span>
									) : null}
								</div>
							);
						})}
					</div>
				))}
			</div>
		</div>
	);
}
