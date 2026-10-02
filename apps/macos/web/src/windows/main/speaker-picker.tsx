import { PlusIcon, UserIcon, UserXIcon } from "lucide-react";
import {
	type ChangeEvent,
	type KeyboardEvent,
	useEffect,
	useRef,
	useState,
} from "react";
import type { MeetingDetailSnapshot, SpeakerOption } from "@/bridge/contract";
import { send, useBridge } from "@/bridge/hooks";
import {
	Button,
	Input,
	Popover,
	PopoverPopup,
	PopoverTrigger,
} from "@/components/ui";
import { menuItemVariants } from "@/components/ui/menu";
import { cn } from "@/lib/cn";

export type Speaker = MeetingDetailSnapshot["speakers"][number];

export const SPEAKER_QUERY_DEBOUNCE_MS = 150;

function optionIcon(kind: SpeakerOption["kind"]) {
	switch (kind) {
		case "person":
			return <UserIcon aria-hidden="true" />;
		case "create":
			return <PlusIcon aria-hidden="true" />;
		case "unknown":
			return <UserXIcon aria-hidden="true" />;
	}
}

export interface SpeakerPickerPanelProps {
	speaker: Speaker;
	/** Called once the pick was sent, so the owner can close. */
	onPicked: () => void;
	className?: string;
}

/**
 * The field and the option list that name one speaker, any speaker: the
 * host is asked for options as the query changes (`speakers.options`) and
 * the pick is sent as `speakers.select`. Mounting it is opening it: the
 * first request runs at once and takes the host's prefill (the suggested
 * name) into the field. `SpeakerPicker` wraps it in a popover for the
 * transcript; the Speakers popover expands it inline under a row.
 */
export function SpeakerPickerPanel({
	speaker,
	onPicked,
	className,
}: SpeakerPickerPanelProps) {
	const client = useBridge();
	const [query, setQuery] = useState("");
	const [options, setOptions] = useState<readonly SpeakerOption[]>([]);
	const [loading, setLoading] = useState(false);
	const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
	const request = useRef(0);

	function load(text: string, prefill: boolean) {
		const id = ++request.current;
		setLoading(true);
		client
			.call("speakers.options", { speakerID: speaker.id, query: text })
			.then((reply) => {
				if (id !== request.current) {
					return;
				}
				setOptions(reply.options);
				if (prefill && reply.prefill) {
					setQuery(reply.prefill);
				}
			})
			.catch((cause: unknown) => {
				console.error("bridge: speakers.options failed", cause);
			})
			.finally(() => {
				if (id === request.current) {
					setLoading(false);
				}
			});
	}

	// biome-ignore lint/correctness/useExhaustiveDependencies: `load` reads the latest speaker id through the closure; the query fires once per speaker.
	useEffect(() => {
		setQuery("");
		setOptions([]);
		load("", true);
		return () => {
			clearTimeout(timer.current);
			request.current += 1;
		};
	}, [speaker.id]);

	function onQueryChange(event: ChangeEvent<HTMLInputElement>) {
		const value = event.target.value;
		setQuery(value);
		clearTimeout(timer.current);
		timer.current = setTimeout(
			() => load(value, false),
			SPEAKER_QUERY_DEBOUNCE_MS,
		);
	}

	function pick(option: SpeakerOption) {
		send(client, "speakers.select", { speakerID: speaker.id, option });
		onPicked();
	}

	function onKeyDown(event: KeyboardEvent<HTMLInputElement>) {
		if (event.key === "Enter" && options[0]) {
			event.preventDefault();
			pick(options[0]);
		}
	}

	return (
		<div className={className}>
			<Input
				aria-label={`Name for ${speaker.clusterLabel}`}
				autoFocus
				data-testid={`speaker-field-${speaker.id}`}
				onChange={onQueryChange}
				onKeyDown={onKeyDown}
				placeholder="Who is this?"
				size="sm"
				value={query}
			/>
			<div
				aria-busy={loading || undefined}
				aria-label="People"
				className="mt-2 flex flex-col gap-0.5"
				role="listbox"
			>
				{options.map((option, index) => (
					<button
						aria-selected={index === 0}
						className={menuItemVariants()}
						key={`${option.kind}-${option.label}`}
						onClick={() => pick(option)}
						role="option"
						type="button"
					>
						{optionIcon(option.kind)}
						<span className="min-w-0 flex-1 truncate">{option.label}</span>
						{option.detail ? (
							<span className="shrink-0 text-faint text-xs">
								{option.detail}
							</span>
						) : null}
					</button>
				))}
				{!loading && options.length === 0 ? (
					<p className="my-0 px-2 py-1.5 text-faint text-xs">
						No one matches yet.
					</p>
				) : null}
			</div>
		</div>
	);
}

export interface SpeakerPickerProps {
	speaker: Speaker;
	open: boolean;
	onOpenChange: (open: boolean) => void;
	className?: string;
}

/**
 * Names a speaker from the transcript: the trigger is the speaker's name
 * on a turn, the popover holds the `SpeakerPickerPanel`. Confirmed
 * speakers open it too, so a wrong name can be taken back where it is read.
 */
export function SpeakerPicker({
	speaker,
	open,
	onOpenChange,
	className,
}: SpeakerPickerProps) {
	return (
		<Popover modal={false} onOpenChange={onOpenChange} open={open}>
			<PopoverTrigger
				className={cn("-ml-[7px] self-start", className)}
				data-testid={`speaker-picker-${speaker.id}`}
				render={<Button size="xs" variant="ghost" />}
			>
				{speaker.displayName}
			</PopoverTrigger>
			<PopoverPopup align="start" padding="sm" size="sm">
				<SpeakerPickerPanel
					onPicked={() => onOpenChange(false)}
					speaker={speaker}
				/>
			</PopoverPopup>
		</Popover>
	);
}
