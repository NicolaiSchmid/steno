import type { BridgeClient } from "@/bridge/client";

/**
 * Deleting a meeting is the one destructive step the page asks the host to
 * confirm natively (Decision 6). The copy matches the SwiftUI dialog.
 */
export async function confirmAndDeleteMeeting(
	client: BridgeClient,
	meeting: { id: string; title: string },
): Promise<boolean> {
	const reply = await client.call("ui.confirmDestructive", {
		title: `Delete “${meeting.title}”?`,
		message:
			"The transcript, summary, tasks and the recording on this Mac are removed. Files already exported to Obsidian stay. People stay.",
		confirmTitle: "Delete",
	});
	if (!reply.confirmed) {
		return false;
	}
	await client.call("meetings.delete", { meetingID: meeting.id });
	return true;
}
