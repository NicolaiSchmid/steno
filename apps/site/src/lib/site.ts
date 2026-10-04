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
	author: { name: "Nicolai Schmid", url: "https://nicolaischmid.com" },
	brew: "brew tap nicolaischmid/tap && brew install --cask steno",
} as const;

/** The desktops in display order: macOS, Windows, Linux. */
export const platformIds = ["mac", "win", "linux"] as const;

export type Platform = (typeof platformIds)[number];

/**
 * One entry per desktop. `released` is true only where a build is on the
 * releases page today; the download button, the closing CTA and the "How
 * it's built" tiles read it, so flipping Windows or Linux here is the whole
 * change once they ship.
 */
export const platforms: Record<
	Platform,
	{ name: string; released: boolean; note: string }
> = {
	mac: {
		name: "macOS",
		released: true,
		note: "Apple Silicon · notarised",
	},
	win: { name: "Windows", released: false, note: "Not released yet" },
	linux: { name: "Linux", released: false, note: "Not released yet" },
};
