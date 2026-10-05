import {
	createContext,
	type ReactNode,
	useContext,
	useEffect,
	useRef,
} from "react";
import type { z } from "zod";
import {
	type PlatformOS,
	type permissionKind,
	platform as platformSchema,
} from "@/bridge/contract";

type PermissionKind = z.infer<typeof permissionKind>;

/**
 * The OS the page runs on and everything the page words or binds for it:
 * the machine ("this Mac", "this computer"), the file manager (Finder,
 * File Explorer), where the privacy switches live, where Steno sits while
 * no window is open, and the shortcut keys (⌘F on the Mac, Ctrl+F
 * elsewhere). Call sites read `usePlatform()` and never branch on the OS
 * themselves; every word that differs lives in `WORDS` below.
 *
 * Where the value comes from, first match wins:
 * - `window.__STENO_PLATFORM__`, which the Tauri shell sets before the
 *   page's scripts run (`apps/desktop/src-tauri/src/platform.rs`);
 * - the `platform` query flag in the hash (`#/main?platform=linux`), for
 *   the dev server, the screens and the tests over the mock bridge;
 * - otherwise the Mac: the Swift app sets nothing, and its pages read
 *   exactly as they did before the other platforms existed.
 */

export type { PlatformOS };

/** The words that differ between the platforms. */
export interface PlatformWords {
	/** The machine after "this", "your" or "the": "Mac", "computer". */
	computer: string;
	/** The button that shows a file or folder in the file manager. */
	showInFileManager: string;
	/** The actions menu item that shows the exported meeting folder. */
	showExport: string;
	/** The actions menu item that shows the recording's files. */
	showRecording: string;
	/** Where a saved API key lives, after "in": "your keychain". */
	keychain: string;
	/** The same in full, after "in": "your login keychain". */
	loginKeychain: string;
	/** Where the privacy switches live, after "in": "System Settings". */
	systemSettings: string;
	/** Whether Steno can open that place itself ("Open System Settings"). */
	opensSystemSettings: boolean;
	/** Where Steno stays without a window, after "Steno runs". */
	runsIn: string;
	/** The always-there control, after "The": "menu bar item". */
	trayItem: string;
	/** Who asks for the system audio recording, and when; Mac only. */
	systemAudioPrompt?: string;
	/** Who asks for local network access, and when. */
	localNetworkPrompt?: string;
}

const WORDS: Record<PlatformOS, PlatformWords> = {
	macos: {
		computer: "Mac",
		showInFileManager: "Show in Finder",
		showExport: "Reveal export",
		showRecording: "Reveal recording",
		keychain: "your keychain",
		loginKeychain: "your login keychain",
		systemSettings: "System Settings",
		opensSystemSettings: true,
		runsIn: "in the menu bar",
		trayItem: "menu bar item",
		systemAudioPrompt: "macOS asks once, during a short test recording.",
		localNetworkPrompt: "macOS asks when you pair the first phone.",
	},
	windows: {
		computer: "computer",
		showInFileManager: "Show in File Explorer",
		showExport: "Show export in File Explorer",
		showRecording: "Show recording in File Explorer",
		keychain: "Credential Manager",
		loginKeychain: "Windows Credential Manager",
		systemSettings: "Settings",
		opensSystemSettings: true,
		runsIn: "in the system tray",
		trayItem: "tray icon",
		localNetworkPrompt:
			"Windows may ask to let Steno through the firewall when you pair the first phone.",
	},
	linux: {
		computer: "computer",
		showInFileManager: "Show in folder",
		showExport: "Show export in folder",
		showRecording: "Show recording in folder",
		keychain: "your keyring",
		loginKeychain: "your keyring",
		systemSettings: "your system settings",
		opensSystemSettings: false,
		runsIn: "in the background",
		trayItem: "tray icon",
	},
};

/**
 * The permissions each OS has, as the host lists them
 * (`Platform::permissions` in `crates/steno-bridge/src/envelope.rs`). The
 * host decides the onboarding and Recording rows; the page needs this only
 * for General's calendar row, whose state the snapshot always carries.
 */
const PERMISSIONS: Record<PlatformOS, readonly PermissionKind[]> = {
	macos: ["microphone", "systemAudio", "calendar", "localNetwork"],
	windows: ["microphone", "localNetwork"],
	linux: ["microphone"],
};

/** A modifier in a shortcut; `mod` is ⌘ on the Mac and Ctrl elsewhere. */
type ShortcutModifier = "mod" | "shift" | "alt";

/**
 * A shortcut: its modifiers in the order the Mac label writes them, then
 * the key as the label shows it ("F", ",").
 */
export interface Shortcut {
	modifiers: readonly ShortcutModifier[];
	key: string;
}

/** The shortcuts the pages show and answer. */
export const SHORTCUTS = {
	/** Focuses the meeting search. */
	findMeetings: { modifiers: ["mod"], key: "F" },
	/** Exports the selected meeting again. */
	exportAgain: { modifiers: ["shift", "mod"], key: "E" },
	/** Starts a call recording, or stops the one running. */
	record: { modifiers: ["mod", "shift"], key: "R" },
} as const satisfies Record<string, Shortcut>;

const MAC_GLYPHS: Record<ShortcutModifier, string> = {
	mod: "⌘",
	shift: "⇧",
	alt: "⌥",
};

/** Windows' and Linux's order: Ctrl, Alt, Shift. */
const PC_ORDER: readonly ShortcutModifier[] = ["mod", "alt", "shift"];
const PC_NAMES: Record<ShortcutModifier, string> = {
	mod: "Ctrl",
	shift: "Shift",
	alt: "Alt",
};

/** "⇧⌘E" on the Mac, "Ctrl+Shift+E" on Windows and Linux. */
export function shortcutLabel(os: PlatformOS, shortcut: Shortcut): string {
	if (os === "macos") {
		return (
			shortcut.modifiers.map((modifier) => MAC_GLYPHS[modifier]).join("") +
			shortcut.key
		);
	}
	return [
		...PC_ORDER.filter((modifier) => shortcut.modifiers.includes(modifier)).map(
			(modifier) => PC_NAMES[modifier],
		),
		shortcut.key,
	].join("+");
}

/** The keys of a key event that a shortcut looks at. */
export interface ShortcutEvent {
	key: string;
	metaKey: boolean;
	ctrlKey: boolean;
	shiftKey: boolean;
	altKey: boolean;
}

/**
 * Whether `event` is `shortcut` on `os`: exactly its modifiers, with ⌘ for
 * `mod` on the Mac and Ctrl elsewhere (so Ctrl+F on the Mac and ⌘F, the
 * Windows key, elsewhere are not it), and the key in either case.
 */
export function matchesShortcut(
	os: PlatformOS,
	event: ShortcutEvent,
	shortcut: Shortcut,
): boolean {
	const has = (modifier: ShortcutModifier) =>
		shortcut.modifiers.includes(modifier);
	const mod = os === "macos" ? event.metaKey : event.ctrlKey;
	const other = os === "macos" ? event.ctrlKey : event.metaKey;
	return (
		mod === has("mod") &&
		!other &&
		event.shiftKey === has("shift") &&
		event.altKey === has("alt") &&
		event.key.toLowerCase() === shortcut.key.toLowerCase()
	);
}

/** The platform as the page uses it. */
export interface Platform {
	os: PlatformOS;
	words: PlatformWords;
	/** The permissions the OS has (see `PERMISSIONS`). */
	permissions: readonly PermissionKind[];
	/**
	 * Whether the page answers the Record shortcut itself. The Swift app's
	 * Record menu owns ⌘⇧R there, and a page that answered it too would
	 * toggle the recorder twice; the Tauri shell has no such menu.
	 */
	bindsRecordShortcut: boolean;
	/** `shortcut`'s label on this platform. */
	label(shortcut: Shortcut): string;
	/** Whether `event` is `shortcut` on this platform. */
	matches(event: ShortcutEvent, shortcut: Shortcut): boolean;
}

function isPlatformOS(value: unknown): value is PlatformOS {
	return platformSchema.safeParse(value).success;
}

/** The platform for `os` in the Tauri shell, which binds Record in the page. */
export function platformFor(os: PlatformOS): Platform {
	return {
		os,
		words: WORDS[os],
		permissions: PERMISSIONS[os],
		bindsRecordShortcut: true,
		label: (shortcut) => shortcutLabel(os, shortcut),
		matches: (event, shortcut) => matchesShortcut(os, event, shortcut),
	};
}

/** The Swift app's Mac, whose Record menu owns the Record shortcut. */
export const SWIFT_MAC: Platform = {
	...platformFor("macos"),
	bindsRecordShortcut: false,
};

/** What the Tauri shell sets (`platform.rs`). */
interface StenoWindow {
	__STENO_PLATFORM__?: unknown;
}

/**
 * The page's platform: the shell's value, else the `platform` query flag
 * (`flag`), else the Swift app's Mac, which leaves the Record shortcut to
 * its menu.
 */
export function detectPlatform(
	target: Window = window,
	flag?: string | null,
): Platform {
	const injected = (target as StenoWindow).__STENO_PLATFORM__;
	if (isPlatformOS(injected)) {
		return platformFor(injected);
	}
	if (isPlatformOS(flag)) {
		return platformFor(flag);
	}
	return SWIFT_MAC;
}

const PlatformContext = createContext<Platform | null>(null);

let pagePlatform: Platform | undefined;

/** Supplies `usePlatform` below it; the app and the tests set one. */
export function PlatformProvider({
	platform,
	children,
}: {
	platform: Platform;
	children?: ReactNode;
}) {
	return (
		<PlatformContext.Provider value={platform}>
			{children}
		</PlatformContext.Provider>
	);
}

/** The provided platform, else the page's own (`detectPlatform`). */
export function usePlatform(): Platform {
	const provided = useContext(PlatformContext);
	if (provided) {
		return provided;
	}
	pagePlatform ??= detectPlatform();
	return pagePlatform;
}

/**
 * Runs `action` when `shortcut` is pressed anywhere in the window, while
 * `enabled`; the key press goes no further. The latest `action` runs, so
 * a caller may pass a fresh closure on every render.
 */
export function useShortcut(
	shortcut: Shortcut,
	action: () => void,
	enabled = true,
): void {
	const platform = usePlatform();
	const latest = useRef(action);
	latest.current = action;
	useEffect(() => {
		if (!enabled) {
			return;
		}
		const onKeyDown = (event: KeyboardEvent) => {
			if (!event.repeat && platform.matches(event, shortcut)) {
				event.preventDefault();
				latest.current();
			}
		};
		window.addEventListener("keydown", onKeyDown);
		return () => window.removeEventListener("keydown", onKeyDown);
	}, [platform, shortcut, enabled]);
}
