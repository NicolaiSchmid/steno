export const site = {
	name: "Steno",
	url: "https://steno.nicolaischmid.com",
	title: "Steno · Meeting notes without the bot",
	description:
		"Steno records your meetings from your computer's own audio, transcribes and summarises them on-device, and writes plain Markdown you own. No bot in the call. No cloud account. Audio never leaves the machine.",
	repo: "https://github.com/NicolaiSchmid/steno",
	fork: "https://github.com/NicolaiSchmid/steno/fork",
	releases: "https://github.com/NicolaiSchmid/steno/releases",
	download: "https://github.com/NicolaiSchmid/steno/releases/latest",
	issues: "https://github.com/NicolaiSchmid/steno/issues",
	tap: "https://github.com/NicolaiSchmid/homebrew-tap",
	scope:
		"https://github.com/NicolaiSchmid/steno/blob/main/.plans/2026-09-24-initial-scope.md",
	/** Site-relative, unlike the URLs above. */
	recordingLaw: "/recording-law",
	author: { name: "Nicolai Schmid", url: "https://nicolaischmid.com" },
	brew: "brew tap nicolaischmid/tap && brew install --cask steno",
} as const;

export type Platform = "mac" | "win" | "linux";

export const platformLabel: Record<Platform, string> = {
	mac: "Download for macOS",
	win: "Download for Windows",
	linux: "Download for Linux",
};
