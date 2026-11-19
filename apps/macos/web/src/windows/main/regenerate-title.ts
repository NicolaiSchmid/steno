import type { BridgeClient } from "@/bridge/client";
import { notify } from "@/components/ui";

/**
 * Asks the host for a new title from the meeting's summary
 * (`meetings.regenerateTitle`). The new title arrives with the list and
 * detail snapshots that follow; a refusal (no summary, summaries off) is a
 * toast, since the list row it came from has no error line.
 */
export function regenerateTitle(client: BridgeClient, meetingID: string): void {
	client
		.call("meetings.regenerateTitle", { meetingID })
		.catch((cause: unknown) => {
			console.error("bridge: meetings.regenerateTitle failed", cause);
			notify({
				title: "Title not regenerated",
				description: cause instanceof Error ? cause.message : String(cause),
				tone: "destructive",
			});
		});
}
