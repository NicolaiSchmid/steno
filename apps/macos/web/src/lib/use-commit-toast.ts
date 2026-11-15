import { useEffect, useRef } from "react";
import { type NotifyOptions, notify } from "@/components/ui";

/**
 * A toast for a commit the user started whose outcome arrives later in a
 * snapshot (an export, a connection test, onboarding's Save). Call the
 * returned `start()` on the click; once `done` turns true after that, the
 * toast shows, once. When `failed` turns true first, nothing shows: the
 * error stays inline where it happened. A `done` that was already true at
 * the click must turn false and back (the host publishes the run), so a
 * stale success never toasts.
 */
export function useCommitToast(
	done: boolean,
	failed: boolean,
	options: NotifyOptions,
): () => void {
	const pending = useRef(false);
	const latest = useRef(options);
	latest.current = options;
	useEffect(() => {
		if (!pending.current) {
			return;
		}
		if (done) {
			pending.current = false;
			notify(latest.current);
		} else if (failed) {
			pending.current = false;
		}
	}, [done, failed]);
	return () => {
		pending.current = true;
	};
}
