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
import type { SpeakerOption } from "@/bridge/contract";
import { send, useBridge } from "@/bridge/hooks";
import { Input } from "@/components/ui";
import { menuItemVariants } from "@/components/ui/menu";
import { groupOptions, optionText, type Speaker } from "./speaker-names";

const SPEAKER_QUERY_DEBOUNCE_MS = 150;

export interface SpeakerNameFieldProps {
	speaker: Speaker;
	/**
	 * Called once with `true` after a pick was sent, with `false` on Escape
	 * (and, with `cancelOnBlur`, when focus leaves the field).
	 */
	onDone: (picked: boolean) => void;
	/** The name the highlighted option would give, for the row's avatar. */
	onPreview?: (name: string | undefined) => void;
	/** Gives up when focus leaves, as a row edited in place does. */
	cancelOnBlur?: boolean;
	className?: string;
}

/**
 * The text field that names one speaker, in place of its name, with the
 * host's options under it in three groups (`groupOptions`). The host is
 * asked as the text changes (`speakers.options`, debounced); arrow keys,
 * Home and End move the highlight, Enter sends the highlighted option as
 * `speakers.select`, Escape gives up. Enter on text the options have not
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

	function pick(option: SpeakerOption) {
		send(client, "speakers.select", { speakerID: speaker.id, option });
		finish(true);
	}

	function onChange(event: ChangeEvent<HTMLInputElement>) {
		const value = event.target.value;
		setQuery(value);
		clearTimeout(timer.current);
		timer.current = setTimeout(() => load(value), SPEAKER_QUERY_DEBOUNCE_MS);
	}

	function onKeyDown(event: KeyboardEvent<HTMLInputElement>) {
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
					pick({ kind: "create", label: typed });
				}
			} else if (highlighted) {
				pick(highlighted);
			}
		} else if (event.key === "Escape") {
			// The field gives up; the popover around it stays open.
			event.preventDefault();
			event.stopPropagation();
			finish(false);
		}
	}

	function onBlur(event: FocusEvent<HTMLDivElement>) {
		if (cancelOnBlur && !event.currentTarget.contains(event.relatedTarget)) {
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
				aria-label={`Name for ${speaker.clusterLabel}`}
				autoFocus
				data-testid={`speaker-field-${speaker.id}`}
				onChange={onChange}
				onKeyDown={onKeyDown}
				placeholder="Type a name"
				role="combobox"
				size="sm"
				value={query}
			/>
			<div
				aria-label="People"
				className="mt-1.5 flex max-h-64 flex-col gap-0.5 overflow-y-auto"
				id={listID}
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
								className="px-2 pt-1.5 pb-0.5 font-medium text-2xs text-faint"
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
									onClick={() => pick(option)}
									onMouseDown={(event) => event.preventDefault()}
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
										<span className="shrink-0 text-faint text-xs">
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
