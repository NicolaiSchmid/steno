import type { PlatformWords } from "@/lib/platform";

/**
 * The six sections, named by what the user gets, not by the subsystem
 * behind it; the same words as `SettingsSection` on the Swift side, whose
 * test pins them.
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

export interface SectionInfo {
	id: SectionId;
	title: string;
	/** One sentence at the top of the section page. */
	purpose: (words: PlatformWords) => string;
}

export const SECTIONS: readonly SectionInfo[] = [
	{
		id: "general",
		title: "General",
		purpose: ({ runsIn }) =>
			`Steno runs ${runsIn} and records when you ask it to.`,
	},
	{
		id: "recording",
		title: "Recording",
		purpose: ({ computer }) =>
			`Audio is recorded and kept on this ${computer} only.`,
	},
	{
		id: "transcription",
		title: "Transcription",
		purpose: ({ computer }) =>
			`Speech is turned into text on this ${computer}. Nothing is uploaded.`,
	},
	{
		id: "summaries",
		title: "Summaries",
		purpose: () =>
			"Meeting summaries and tasks are written by an AI model you choose. Only the transcript text is sent to it.",
	},
	{
		id: "export",
		title: "Export",
		purpose: () =>
			"Finished meetings can be written into an Obsidian vault as notes you own.",
	},
	{
		id: "iphone",
		title: "iPhone",
		purpose: ({ computer }) =>
			`Record on your iPhone when you are away from the ${computer}. Recordings travel over your Wi-Fi only, encrypted to this ${computer}.`,
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
