import { PlusIcon, XIcon } from "lucide-react";
import { type KeyboardEvent, useState } from "react";
import { send, useBridge } from "@/bridge/hooks";
import {
	Button,
	Input,
	Popover,
	PopoverDescription,
	PopoverPopup,
	PopoverTitle,
	PopoverTrigger,
} from "@/components/ui";

export interface TagEditorProps {
	tags: readonly string[];
}

/** `#Strategie ` → `strategie`. */
export function normaliseTag(raw: string): string {
	return raw.trim().replace(/^#+/, "").trim().toLowerCase();
}

/**
 * The "Add tag" button beside the meeting's tags: a popover with the current
 * tags (each removable) and a field that adds one on Return. Every change is
 * one `meeting.setTags` with the full list.
 */
export function TagEditor({ tags }: TagEditorProps) {
	const client = useBridge();
	const [open, setOpen] = useState(false);
	const [draft, setDraft] = useState("");

	function save(next: readonly string[]) {
		send(client, "meeting.setTags", { tags: [...next] });
	}

	function add() {
		const tag = normaliseTag(draft);
		if (!tag) {
			return;
		}
		if (!tags.includes(tag)) {
			save([...tags, tag]);
		}
		setDraft("");
	}

	function onKeyDown(event: KeyboardEvent<HTMLInputElement>) {
		if (event.key === "Enter") {
			event.preventDefault();
			add();
		}
	}

	return (
		<Popover modal={false} onOpenChange={setOpen} open={open}>
			<PopoverTrigger
				aria-label={tags.length === 0 ? undefined : "Edit tags"}
				data-testid="edit-tags"
				render={<Button size="xs" variant="ghost-muted" />}
			>
				<PlusIcon aria-hidden="true" />
				{tags.length === 0 ? "Add tag" : null}
			</PopoverTrigger>
			<PopoverPopup align="start">
				<PopoverTitle>Tags</PopoverTitle>
				<PopoverDescription>
					Tags group meetings in the sidebar and travel with the export.
				</PopoverDescription>
				{tags.length > 0 ? (
					<div className="mt-3 flex flex-wrap gap-1.5">
						{tags.map((tag) => (
							<Button
								aria-label={`Remove tag ${tag}`}
								data-testid={`remove-tag-${tag}`}
								key={tag}
								onClick={() => save(tags.filter((other) => other !== tag))}
								size="xs"
								variant="outline"
							>
								#{tag}
								<XIcon aria-hidden="true" />
							</Button>
						))}
					</div>
				) : null}
				<Input
					aria-label="New tag"
					autoFocus
					className="mt-3"
					data-testid="tags-field"
					onChange={(event) => setDraft(event.target.value)}
					onKeyDown={onKeyDown}
					placeholder="Add a tag and press Return"
					size="sm"
					value={draft}
				/>
			</PopoverPopup>
		</Popover>
	);
}
