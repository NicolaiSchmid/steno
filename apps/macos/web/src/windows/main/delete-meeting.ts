import type { BridgeClient } from "@/bridge/client";

/**
 * Asks the host to delete a meeting. The host shows the confirmation alert
 * itself (Decision 6) and replies with whether the user confirmed; the page
 * changes nothing either way, the list snapshot that follows does.
 */
export async function deleteMeeting(
	client: BridgeClient,
	meetingID: string,
): Promise<boolean> {
	const reply = await client.call("meetings.delete", { meetingID });
	return reply.confirmed;
}
