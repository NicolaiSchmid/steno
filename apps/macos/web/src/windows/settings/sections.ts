import type { BridgeTopic } from "@/bridge/contract";

/**
 * The six sections, named by what the user gets, not by the subsystem
 * behind it; the same words as `SettingsSection` on the Swift side, whose
 * test pins them. The subtitle under each title comes from the section's
 * snapshot, so the page never derives status text itself.
 */
export const SECTION_IDS = [
	"general",
	"recording",
	"transcription",
	"summaries",
	"export",
	"iphone",
] as const;

export type SectionId = (typeof SECTION_IDS)[number];

/** The six `settings.*` topics, each with the `subtitle` the sidebar shows. */
export type SettingsTopic = Extract<BridgeTopic, `settings.${string}`>;

export interface SectionInfo {
	id: SectionId;
	title: string;
	/** One sentence under the section title. */
	purpose: string;
	/** The snapshot topic whose `subtitle` the sidebar shows. */
	topic: SettingsTopic;
}

export const SECTIONS: readonly SectionInfo[] = [
	{
		id: "general",
		title: "General",
		purpose: "Steno runs in the menu bar and records when you ask it to.",
		topic: "settings.general",
	},
	{
		id: "recording",
		title: "Recording",
		purpose: "Audio is recorded and kept on this Mac only.",
		topic: "settings.recording",
	},
	{
		id: "transcription",
		title: "Transcription",
		purpose: "Speech is turned into text on this Mac. Nothing is uploaded.",
		topic: "settings.transcription",
	},
	{
		id: "summaries",
		title: "Summaries",
		purpose:
			"Meeting summaries and tasks are written by an AI model you choose. Only the transcript text is sent to it.",
		topic: "settings.summaries",
	},
	{
		id: "export",
		title: "Export",
		purpose:
			"Finished meetings can be written into an Obsidian vault as notes you own.",
		topic: "settings.export",
	},
	{
		id: "iphone",
		title: "iPhone",
		purpose:
			"Record on your iPhone when you are away from the Mac. Recordings travel over your Wi-Fi only, encrypted to this Mac.",
		topic: "settings.iphone",
	},
];

export function isSectionId(
	value: string | null | undefined,
): value is SectionId {
	return SECTION_IDS.some((id) => id === value);
}

export function sectionInfo(id: SectionId): SectionInfo {
	const info = SECTIONS.find((section) => section.id === id);
	if (!info) {
		throw new Error(`unknown settings section ${id}`);
	}
	return info;
}
