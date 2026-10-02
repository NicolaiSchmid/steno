import { ChevronDownIcon, PlayIcon, SquareIcon } from "lucide-react";
import { useState } from "react";
import type { MeetingDetailSnapshot } from "@/bridge/contract";
import { send, useBridge } from "@/bridge/hooks";
import {
	Avatar,
	AvatarStack,
	Badge,
	Button,
	Popover,
	PopoverPopup,
	PopoverTitle,
	PopoverTrigger,
} from "@/components/ui";
import { formatPeople } from "./format";
import { type Speaker, SpeakerPickerPanel } from "./speaker-picker";

export interface SpeakersPopoverProps {
	speakers: MeetingDetailSnapshot["speakers"];
}

/**
 * The header's Speakers row as the entry point to naming: the avatar stack
 * and the names are one button that opens a popover with one row per
 * speaker in cluster order (avatar, name, email, a Play button for the
 * sample clip). Clicking a name, confirmed or not, expands the picker under
 * the row, so a speaker can be renamed at any time; the pick applies at
 * once through `speakers.select` and the store redraws the rows.
 */
export function SpeakersPopover({ speakers }: SpeakersPopoverProps) {
	const client = useBridge();
	const [open, setOpen] = useState(false);
	const [expanded, setExpanded] = useState<string | null>(null);

	function onOpenChange(next: boolean) {
		setOpen(next);
		if (!next) {
			setExpanded(null);
		}
	}

	function togglePlay(speaker: Speaker) {
		if (speaker.isPlaying) {
			send(client, "speakers.stop");
		} else {
			send(client, "speakers.play", { speakerID: speaker.id });
		}
	}

	return (
		<Popover modal={false} onOpenChange={onOpenChange} open={open}>
			<PopoverTrigger
				aria-label="Speakers"
				className="-ml-[7px] gap-2"
				data-testid="speakers-trigger"
				render={<Button size="sm" variant="ghost-muted" />}
			>
				<AvatarStack ring="background">
					{speakers.map((speaker) => (
						<Avatar
							index={speaker.colorIndex}
							key={speaker.id}
							name={speaker.displayName}
							size="md"
							unknown={speaker.assignment === "unknown"}
						/>
					))}
				</AvatarStack>
				<span className="text-sm">{formatPeople(speakers)}</span>
				<ChevronDownIcon aria-hidden="true" />
			</PopoverTrigger>
			<PopoverPopup align="start" size="lg">
				<PopoverTitle>Speakers</PopoverTitle>
				<ul className="my-0 mt-3 flex list-none flex-col gap-1 p-0">
					{speakers.map((speaker) => {
						const isExpanded = expanded === speaker.id;
						const unconfirmed = speaker.assignment !== "confirmed";
						return (
							<li
								className="flex flex-col"
								data-testid={`speaker-row-${speaker.id}`}
								key={speaker.id}
							>
								<div className="flex items-center gap-3">
									<Avatar
										index={speaker.colorIndex}
										name={speaker.displayName}
										size="lg"
										unknown={speaker.assignment === "unknown"}
									/>
									<Button
										aria-expanded={isExpanded}
										className="-ml-[9px] min-w-0 flex-1 justify-start"
										data-testid={`speaker-picker-${speaker.id}`}
										onClick={() => setExpanded(isExpanded ? null : speaker.id)}
										size="sm"
										variant="ghost"
									>
										<span className="truncate font-medium">
											{speaker.displayName}
										</span>
										{speaker.email ? (
											<span className="truncate font-normal text-faint">
												{speaker.email}
											</span>
										) : null}
										{unconfirmed ? (
											<Badge className="shrink-0" size="sm" variant="warning">
												{speaker.assignment === "suggested"
													? "Suggested"
													: "Who is this?"}
											</Badge>
										) : null}
										<ChevronDownIcon
											aria-hidden="true"
											className="ml-auto shrink-0"
										/>
									</Button>
									{speaker.hasClip ? (
										<Button
											aria-label={
												speaker.isPlaying
													? `Stop the sample of ${speaker.displayName}`
													: `Play a sample of ${speaker.displayName}`
											}
											data-testid={`speaker-play-${speaker.id}`}
											onClick={() => togglePlay(speaker)}
											size="icon-sm"
											variant="ghost"
										>
											{speaker.isPlaying ? (
												<SquareIcon aria-hidden="true" />
											) : (
												<PlayIcon aria-hidden="true" />
											)}
										</Button>
									) : null}
								</div>
								{isExpanded ? (
									<SpeakerPickerPanel
										className="mt-2 mb-1 ml-11"
										onPicked={() => setExpanded(null)}
										speaker={speaker}
									/>
								) : null}
							</li>
						);
					})}
				</ul>
			</PopoverPopup>
		</Popover>
	);
}
