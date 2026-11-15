import { PlusIcon, UserIcon } from "lucide-react";
import {
	type ChangeEvent,
	type FocusEvent,
	type KeyboardEvent,
	useEffect,
	useId,
	useRef,
	useState,
} from "react";
import type { BridgeClient } from "@/bridge/client";
import type { SpeakerOption } from "@/bridge/contract";
import { useBridge } from "@/bridge/hooks";
import { BridgeError } from "@/bridge/transport";
import { Input } from "@/components/ui";
import { menuItemVariants } from "@/components/ui/menu";
import { cn } from "@/lib/cn";
import { groupOptions, optionText, type Speaker } from "./speaker-names";

const SPEAKER_QUERY_DEBOUNCE_MS = 150;

export interface SpeakerNameFieldProps {
	speaker: Speaker;
	/**
	 * Called once with `true` after the host took a pick, with `false` on
	 * Escape (and, with `cancelOnBlur`, when focus leaves the field). A
	 * cancelled combine or a failed pick keeps the field open.
	 */
	onDone: (picked: boolean) => void;
	/** The name the highlighted option would give, for the row's avatar. */
	onPreview?: (name: string | undefined) => void;
	/** Gives up when focus leaves, as a row edited in place does. */
	cancelOnBlur?: boolean;
	className?: string;
}

/**
 * How a pick ended: the host took it, the user cancelled the combine, or it
 * failed with the host's message.
 */
export type SelectOutcome =
	| { kind: "committed" }
	| { kind: "cancelled" }
	| { kind: "failed"; message: string };

/**
 * Sends a pick as `speakers.select` without `combine`. When the host
 * refuses it as `wouldCombine` (the person already owns another voice of
 * the meeting, so naming would merge the two), the user confirms first and
 * the pick goes again with `combine: true`; Cancel leaves it. Never
 * rejects: any failure comes back as `failed`.
 */
export async function selectSpeaker(
	client: BridgeClient,
	speaker: Speaker,
	option: SpeakerOption,
): Promise<SelectOutcome> {
	const params = { speakerID: speaker.id, option };
	try {
		try {
			await client.call("speakers.select", { ...params, combine: false });
		} catch (cause) {
			if (!(cause instanceof BridgeError) || cause.code !== "wouldCombine") {
				throw cause;
			}
			const { confirmed } = await client.call("ui.confirmDestructive", {
				title: `Combine ${speaker.clusterLabel} with ${option.label}?`,
				message: `${cause.message} Their paragraphs become ${option.label}’s. This cannot be undone.`,
				confirmTitle: "Combine",
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

/** The host's message for a failed pick, under the field or row it came from. */
export function SelectError({
	message,
	className,
}: {
	message: string | undefined;
	className?: string;
}) {
	return message ? (
		<p
			className={cn(
				"mt-1.5 mb-0 text-destructive-foreground text-xs",
				className,
			)}
			role="alert"
		>
			{message}
		</p>
	) : null;
}

/**
 * The text field that names one speaker, in place of its name, with the
 * host's options under it in three groups (`groupOptions`). The host is
 * asked as the text changes (`speakers.options`, debounced); arrow keys,
 * Home and End move the highlight, Enter sends the highlighted option
 * (`selectSpeaker`), Escape gives up. The field closes once the host took
 * the pick; a cancelled combine leaves it as it was, and a failure shows
 * the host's message under it. Enter on text the options have not
 * caught up with yet creates the typed name, which the host resolves to an
 * existing person of that name. Opens empty: a suggestion is confirmed
 * with its own button, not by Enter on a prefilled field.
 */
export function SpeakerNameField({
	speaker,
	onDone,
	onPreview,
	cancelOnBlur = false,
	className,
}: SpeakerNameFieldProps) {
	const client = useBridge();
	const id = useId();
	const [query, setQuery] = useState("");
	const [answered, setAnswered] = useState<{
		query: string;
		options: readonly SpeakerOption[];
	}>({ query: "", options: [] });
	const [active, setActive] = useState(0);
	const [error, setError] = useState<string | undefined>(undefined);
	const input = useRef<HTMLInputElement>(null);
	const picking = useRef(false);
	const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
	const request = useRef(0);
	const done = useRef(false);

	function load(text: string) {
		const ticket = ++request.current;
		client
			.call("speakers.options", { speakerID: speaker.id, query: text })
			.then((reply) => {
				if (ticket === request.current) {
					setAnswered({ query: text, options: reply.options });
					setActive(0);
				}
			})
			.catch((cause: unknown) => {
				console.error("bridge: speakers.options failed", cause);
			});
	}

	// biome-ignore lint/correctness/useExhaustiveDependencies: one first request per speaker; `load` reads the latest id through the closure.
	useEffect(() => {
		load("");
		return () => {
			clearTimeout(timer.current);
			request.current += 1;
		};
	}, [speaker.id]);

	const groups = groupOptions(answered.options, answered.query);
	const flat = groups.flatMap((group) => group.options);
	const highlightedAt = Math.min(active, flat.length - 1);
	const highlighted = flat[highlightedAt];

	// biome-ignore lint/correctness/useExhaustiveDependencies: the preview follows the highlighted row only.
	useEffect(() => {
		onPreview?.(highlighted?.label);
	}, [highlighted?.label]);

	function finish(picked: boolean) {
		if (done.current) {
			return;
		}
		done.current = true;
		onPreview?.(undefined);
		onDone(picked);
	}

	async function pick(option: SpeakerOption) {
		if (picking.current) {
			return;
		}
		picking.current = true;
		setError(undefined);
		const outcome = await selectSpeaker(client, speaker, option);
		picking.current = false;
		if (outcome.kind === "committed") {
			finish(true);
			return;
		}
		if (outcome.kind === "failed") {
			setError(outcome.message);
		}
		// The confirm sheet took focus; the same speaker's edit goes on.
		input.current?.focus();
	}

	function onChange(event: ChangeEvent<HTMLInputElement>) {
		const value = event.target.value;
		setQuery(value);
		setError(undefined);
		clearTimeout(timer.current);
		timer.current = setTimeout(() => load(value), SPEAKER_QUERY_DEBOUNCE_MS);
	}

	function onKeyDown(event: KeyboardEvent<HTMLInputElement>) {
		// WebKit sends the Enter that ends a composition as keyCode 229.
		if (event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) {
			return;
		}
		const last = flat.length - 1;
		const move: Record<string, number> = {
			ArrowDown: Math.min(active + 1, last),
			ArrowUp: Math.max(active - 1, 0),
			Home: 0,
			End: last,
		};
		if (event.key in move && flat.length > 0) {
			event.preventDefault();
			setActive(move[event.key] ?? 0);
		} else if (event.key === "Enter") {
			event.preventDefault();
			const typed = query.trim();
			if (answered.query.trim() !== typed) {
				// The options are for older text; create what was typed.
				if (typed) {
					void pick({ kind: "create", label: typed });
				}
			} else if (highlighted) {
				void pick(highlighted);
			}
		} else if (event.key === "Escape") {
			// The field gives up; the popover around it stays open.
			event.preventDefault();
			event.stopPropagation();
			finish(false);
		}
	}

	function onBlur(event: FocusEvent<HTMLDivElement>) {
		// A pick in flight may open the host's confirm, which takes focus.
		if (
			cancelOnBlur &&
			!picking.current &&
			!event.currentTarget.contains(event.relatedTarget)
		) {
			finish(false);
		}
	}

	const listID = `${id}-options`;
	const optionID = (index: number) => `${id}-option-${index}`;

	return (
		// biome-ignore lint/a11y/noStaticElementInteractions: focus leaving the field and its options, together, ends editing.
		<div className={className} onBlur={onBlur}>
			<Input
				aria-activedescendant={
					highlighted ? optionID(highlightedAt) : undefined
				}
				aria-autocomplete="list"
				aria-controls={listID}
				aria-expanded={flat.length > 0}
				aria-invalid={error ? true : undefined}
				aria-label={`Name for ${speaker.clusterLabel}`}
				autoFocus
				data-testid={`speaker-field-${speaker.id}`}
				onChange={onChange}
				onKeyDown={onKeyDown}
				placeholder="Type a name"
				ref={input}
				role="combobox"
				size="sm"
				value={query}
			/>
			<SelectError message={error} />
			<div
				aria-label="People"
				className="mt-1.5 flex max-h-64 flex-col gap-0.5 overflow-y-auto"
				id={listID}
				// Clicks in the list keep focus in the field, so the edit goes on.
				onMouseDown={(event) => event.preventDefault()}
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
						{group.options.map((option) => {
							const at = flat.indexOf(option);
							const isActive = option === highlighted;
							return (
								// biome-ignore lint/a11y/useKeyWithClickEvents: focus stays in the combobox, which owns the keys (aria-activedescendant).
								<div
									aria-selected={isActive}
									className={menuItemVariants()}
									data-highlighted={isActive ? "" : undefined}
									id={optionID(at)}
									key={`${option.kind}-${option.personID ?? option.label}`}
									onClick={() => void pick(option)}
									onMouseMove={() => setActive(at)}
									role="option"
									tabIndex={-1}
								>
									{option.kind === "person" ? (
										<UserIcon aria-hidden="true" />
									) : (
										<PlusIcon aria-hidden="true" />
									)}
									<span className="min-w-0 flex-1 truncate">
										{optionText(option)}
									</span>
									{option.detail ? (
										<span className="shrink-0 text-meta text-muted-foreground">
											{option.detail}
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
