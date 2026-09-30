import type { BridgeClient } from "@/bridge/client";

/**
 * Asks the host to delete a meeting. The host shows the confirmation alert
 * itself (Decision 6) and replies with whether the user confirmed; the page
 * changes nothing either way, the list snapshot that follows does. A failed
 * call is logged, not surfaced: the host has already told the user.
 */
export function deleteMeeting(client: BridgeClient, meetingID: string): void {
	client.call("meetings.delete", { meetingID }).catch((cause: unknown) => {
		console.error("bridge: delete failed", cause);
	});
}
