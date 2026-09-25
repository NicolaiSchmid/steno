import { View } from "react-native";

import { AppText } from "@/components/AppText";
import type { SyncState } from "@/features/queue/queue-index";

/**
 * One chip per sync state: a status-coloured dot and label on the card veil
 * (alpha chips, never filled). Colours are the semantic tokens in global.css.
 */
const STATES: Record<SyncState, { label: string; dot: string; text: string }> =
	{
		recording: {
			label: "Recording",
			dot: "bg-destructive",
			text: "text-destructive",
		},
		queued: {
			label: "Waiting for Mac",
			dot: "bg-warning",
			text: "text-warning",
		},
		uploading: { label: "Uploading", dot: "bg-info", text: "text-info" },
		delivered: {
			label: "Delivered",
			dot: "bg-live-bright",
			text: "text-live-bright",
		},
		failed: {
			label: "Failed",
			dot: "bg-destructive",
			text: "text-destructive",
		},
		unpaired: { label: "Not paired", dot: "bg-faint", text: "text-faint" },
	};

export function SyncStatusBadge({
	state,
	detail,
}: {
	state: SyncState;
	/** Replaces the label, e.g. "Uploading 37%". */
	detail?: string;
}) {
	const style = STATES[state];
	return (
		<View
			accessibilityLabel={detail ?? style.label}
			accessibilityRole="text"
			className="flex-row items-center gap-1.5 rounded-full bg-card px-2.5 py-1"
		>
			<View className={`h-1.5 w-1.5 rounded-full ${style.dot}`} />
			<AppText className={style.text} variant="meta">
				{detail ?? style.label}
			</AppText>
		</View>
	);
}
