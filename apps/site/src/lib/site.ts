export const site = {
	name: "Steno",
	url: "https://steno.nicolaischmid.com",
	title: "Steno · Meeting notes without the bot",
	description:
		"Steno records your meetings from your computer's own audio, transcribes them on-device, summarises them with the model you choose, and writes plain Markdown you own. No bot in the call. No cloud account. Audio never leaves the machine.",
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

/** The desktops in display order: macOS, Windows, Linux. */
export const platformIds = ["mac", "win", "linux"] as const;

export type Platform = (typeof platformIds)[number];

/**
 * Each desktop, and each Linux target, is listed once its gate in
 * `.plans/2026-10-07-stable-promotion.md` has passed on a real machine (D6,
 * "The Linux gates"). Flipping its flag is the whole change here; when that
 * adds a desktop, the root README's desktop badge changes in the same commit.
 * A Rollback that takes `v0.11.0` out of "latest" sets every flag back to
 * false ("A bad release" in `apps/desktop/README.md` says why).
 */
const macReleased = true;
const windowsReleased = false;

interface LinuxTarget {
	name: string;
	/** How it is installed, shown in the note as "GNOME (.deb · AppImage)". */
	via: string;
	released: boolean;
	install: string;
	/** False for a line in a config file: no `$` prompt. */
	shell?: false;
}

/** Linux counts as released once one target is. */
const linuxTargets: LinuxTarget[] = [
	{
		name: "GNOME",
		via: ".deb · AppImage",
		released: false,
		install: "sudo apt install ./steno-desktop_*_amd64.deb",
	},
	{
		name: "Omarchy",
		via: "AUR",
		released: false,
		install: "yay -S steno-desktop-bin",
	},
	{
		name: "NixOS",
		via: "flake",
		released: false,
		install: "programs.steno.enable = true;",
		shell: false,
	},
];

export const listedLinuxTargets = linuxTargets.filter((t) => t.released);

/**
 * Every release carries every desktop's build, so an unlisted desktop's note
 * says only that its build is not yet checked.
 */
function desktop(name: string, released: boolean, note: string) {
	return {
		name,
		released,
		note: released ? note : "In each release · not yet checked",
	};
}

/**
 * One entry per desktop. The download button reads `released`; the closing
 * CTA and the "How it's built" tiles show `note`.
 */
export const platforms: Record<
	Platform,
	{ name: string; released: boolean; note: string }
> = {
	mac: desktop("macOS", macReleased, "Apple Silicon · notarised"),
	win: desktop("Windows", windowsReleased, "x64 · installer not code-signed"),
	linux: desktop(
		"Linux",
		listedLinuxTargets.length > 0,
		listedLinuxTargets.map((t) => `${t.name} (${t.via})`).join(" · "),
	),
};
