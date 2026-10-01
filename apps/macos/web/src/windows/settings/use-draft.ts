import { useEffect, useState } from "react";

/**
 * A text field's draft over a snapshot value. Typing changes the draft
 * alone; the caller sends it to the host on blur or Return, and the host's
 * next snapshot (which echoes what it stored) resets the draft, so the field
 * never fights the round trip keystroke by keystroke.
 */
export function useDraft(value: string): [string, (next: string) => void] {
	const [draft, setDraft] = useState(value);
	useEffect(() => {
		setDraft(value);
	}, [value]);
	return [draft, setDraft];
}
