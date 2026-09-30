import { type ChangeEvent, useEffect, useRef, useState } from "react";
import { send, useBridge } from "@/bridge/hooks";
import { Textarea } from "@/components/ui";

export const NOTES_DEBOUNCE_MS = 1000;

export interface NotesTabProps {
	meetingID: string;
	notes: string;
}

/**
 * Your own notes for the meeting. Typing saves a second after the last
 * keystroke; leaving the tab or the meeting saves at once and asks the
 * host to flush. The host's snapshot is adopted only while nothing is
 * pending, so an echo never overwrites what is being typed.
 */
export function NotesTab({ meetingID, notes }: NotesTabProps) {
	const client = useBridge();
	const [text, setText] = useState(notes);
	const latest = useRef(notes);
	const dirty = useRef(false);
	const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

	useEffect(() => {
		if (!dirty.current) {
			latest.current = notes;
			setText(notes);
		}
	}, [notes]);

	useEffect(() => {
		return () => {
			clearTimeout(timer.current);
			if (dirty.current) {
				dirty.current = false;
				send(client, "meeting.saveNotes", { meetingID, text: latest.current });
			}
			send(client, "meeting.flushNotes", { meetingID });
		};
	}, [client, meetingID]);

	function onChange(event: ChangeEvent<HTMLTextAreaElement>) {
		const value = event.target.value;
		latest.current = value;
		dirty.current = true;
		setText(value);
		clearTimeout(timer.current);
		timer.current = setTimeout(() => {
			dirty.current = false;
			send(client, "meeting.saveNotes", { meetingID, text: latest.current });
		}, NOTES_DEBOUNCE_MS);
	}

	return (
		<div className="flex flex-col gap-2" data-testid="tab-content-notes">
			<Textarea
				aria-label="Notes"
				data-testid="scratchpad-editor"
				onChange={onChange}
				placeholder="Anything you want to keep with this meeting."
				rows={14}
				spellCheck
				value={text}
			/>
			<p className="my-0 text-faint text-xs" data-testid="scratchpad-hint">
				Saved with the meeting and exported alongside the summary.
			</p>
		</div>
	);
}
