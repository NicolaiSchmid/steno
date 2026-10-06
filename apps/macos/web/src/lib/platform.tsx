import {
	createContext,
	type ReactNode,
	useContext,
	useEffect,
	useRef,
} from "react";
import { type PlatformOS, platform as platformSchema } from "@/bridge/contract";

/**
 * The OS the page runs on and everything the page words or binds for it:
 * the machine ("this Mac", "this computer"), the file manager (Finder,
 * File Explorer), where the privacy switches live, where Steno sits while
 * no window is open, the shortcut keys (⌘F on the Mac, Ctrl+F elsewhere),
 * and whether the window's title bar lies over the page. Call sites read
 * `usePlatform()` and never branch on the OS themselves; every word that
 * differs lives in `WORDS` below, every layout difference in the records
 * beside it.
 *
 * Where the value comes from, first match wins:
 * - `window.__STENO_PLATFORM__`, which the Tauri shell sets before the
 *   page's scripts run (`apps/desktop/src-tauri/src/platform.rs`);
 * - the `platform` query flag in the hash (`#/main?platform=linux`), for
 *   the dev server, the screens and the tests over the mock bridge;
 * - otherwise the Mac: the Swift app sets nothing, and its pages keep
 *   the Mac's words.
 */

export type { PlatformOS };

/** The words that differ between the platforms. */
export interface PlatformWords {
	/** The machine after "this", "your" or "the": "Mac", "computer". */
	computer: string;
	/**
	 * Settings > Recording's button that shows the recordings folder in
	 * the file manager.
	 */
	showInFileManager: string;
	/**
	 * The meeting footer's button that shows the export in the file
	 * manager; the Mac's keeps the Swift app's "Reveal in Finder".
	 */
	revealInFileManager: string;
	/** The actions menu item that shows the exported meeting folder. */
	showExport: string;
	/** The actions menu item that shows the recording's files. */
	showRecording: string;
	/**
	 * Where a saved API key lives, after "in": "your keychain", "a file
	 * only you can read" (`crates/steno-services/src/secrets.rs`).
	 */
	keychain: string;
	/** The same in full, after "in": "your login keychain". */
	loginKeychain: string;
	/**
	 * Where the privacy switches live, after "in": "System Settings", and
	 * after "Open" and "Fix in" where `opensSystemSettings`.
	 */
	systemSettings: string;
	/** Whether Steno can open that place itself ("Open System Settings"). */
	opensSystemSettings: boolean;
	/**
	 * Where Steno stays without a window, after "Steno runs"; none on
	 * Linux, whose tray shows only where a status notifier host runs.
	 */
	runsIn?: string;
	/** The always-there control, after "The": "menu bar item"; none on Linux. */
	trayItem?: string;
	/** The file the Codex command-line tool keeps its sign-in in. */
	codexSignInFile: string;
	/** Who asks for the system audio recording, and when; Mac only. */
	systemAudioPrompt?: string;
	/**
	 * Who asks for local network access, and when; none on Linux, which
	 * lists no local network step.
	 */
	localNetworkPrompt?: string;
}

const WORDS: Record<PlatformOS, PlatformWords> = {
	macos: {
		computer: "Mac",
		showInFileManager: "Show in Finder",
		revealInFileManager: "Reveal in Finder",
		showExport: "Reveal export",
		showRecording: "Reveal recording",
		keychain: "your keychain",
		loginKeychain: "your login keychain",
		systemSettings: "System Settings",
		opensSystemSettings: true,
		runsIn: "in the menu bar",
		trayItem: "menu bar item",
		codexSignInFile: "~/.codex/auth.json",
		systemAudioPrompt: "macOS asks once, during a short test recording.",
		localNetworkPrompt: "macOS asks when you pair the first phone.",
	},
	windows: {
		computer: "computer",
		showInFileManager: "Show in File Explorer",
		revealInFileManager: "Show in File Explorer",
		showExport: "Show export in File Explorer",
		showRecording: "Show recording in File Explorer",
		keychain: "Credential Manager",
		loginKeychain: "Windows Credential Manager",
		systemSettings: "Windows Settings",
		opensSystemSettings: true,
		runsIn: "in the system tray",
		trayItem: "tray icon",
		codexSignInFile: "%USERPROFILE%\\.codex\\auth.json",
		localNetworkPrompt:
			"Windows may ask to let Steno through the firewall when you pair the first phone.",
	},
	linux: {
		computer: "computer",
		showInFileManager: "Show in folder",
		revealInFileManager: "Show in folder",
		showExport: "Show export in folder",
		showRecording: "Show recording in folder",
		keychain: "a file only you can read",
		loginKeychain: "a file only you can read",
		systemSettings: "your system settings",
		opensSystemSettings: false,
		codexSignInFile: "~/.codex/auth.json",
	},
};

/**
 * Whether Steno reads a calendar on the OS: whether the host's list
 * (`PermissionKind::for_platform` in `crates/steno-bridge/src/envelope.rs`)
 * has the calendar. The host decides the onboarding and Recording rows; the
 * page needs this only for General's calendar row, whose state the snapshot
 * always carries. `crates/steno-bridge/tests/fixtures.rs` compares this
 * record's text with that list.
 */
const READS_CALENDAR: Record<PlatformOS, boolean> = {
	macos: true,
	windows: false,
	linux: false,
};

/**
 * Whether the window's title bar lies over the top of the page, so the
 * page leaves the traffic lights their room: the sidebar's header-high
 * spacer and onboarding's 52 px top. On the Mac the Swift windows and the
 * Tauri shell's (`TitleBarStyle::Overlay` in
 * `apps/desktop/src-tauri/src/windows.rs`) paint the page up to the top
 * edge; on Windows and Linux the shell keeps the native title bar above
 * the page, and the inset would be an empty band.
 */
const TITLE_BAR_INSET: Record<PlatformOS, boolean> = {
	macos: true,
	windows: false,
	linux: false,
};

/** A modifier in a shortcut; `mod` is ⌘ on the Mac and Ctrl elsewhere. */
type ShortcutModifier = "mod" | "shift";

/**
 * A shortcut: its modifiers in the order the Mac label writes them, then
 * the key as the label shows it ("F").
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
};

/** Windows' and Linux's order: Ctrl, then Shift. */
const ELSEWHERE_ORDER: readonly ShortcutModifier[] = ["mod", "shift"];
const ELSEWHERE_NAMES: Record<ShortcutModifier, string> = {
	mod: "Ctrl",
	shift: "Shift",
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
		...ELSEWHERE_ORDER.filter((modifier) =>
			shortcut.modifiers.includes(modifier),
		).map((modifier) => ELSEWHERE_NAMES[modifier]),
		shortcut.key,
	].join("+");
}

/** The keys of a key event that a shortcut looks at. */
export interface ShortcutEvent {
	key: string;
	code: string;
	metaKey: boolean;
	ctrlKey: boolean;
	shiftKey: boolean;
	altKey: boolean;
}

/** The physical key's `code` for a shortcut's letter key: "KeyF". */
function keyCode(key: string): string | undefined {
	return /^[A-Za-z]$/.test(key) ? `Key${key.toUpperCase()}` : undefined;
}

/**
 * Whether the event's key is `key`: the character in either case, or,
 * where the layout types no ASCII character there (Cyrillic, Greek), the
 * physical key that types it on a US layout.
 */
function keyMatches(event: ShortcutEvent, key: string): boolean {
	if (/^[\x20-\x7e]$/.test(event.key)) {
		return event.key.toLowerCase() === key.toLowerCase();
	}
	return event.code === keyCode(key);
}

/**
 * Whether `event` is `shortcut` on `os`: exactly its modifiers (and no
 * Alt), with ⌘ for `mod` on the Mac and Ctrl elsewhere (so Ctrl+F on the
 * Mac, and the Windows or Super key with F elsewhere, do not match), and
 * its key (`keyMatches`).
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
		!event.altKey &&
		keyMatches(event, shortcut.key)
	);
}

/** The platform as the page uses it. */
export interface Platform {
	os: PlatformOS;
	words: PlatformWords;
	/** Whether Steno reads a calendar here (see `READS_CALENDAR`). */
	readsCalendar: boolean;
	/** Whether the page leaves room for the traffic lights (`TITLE_BAR_INSET`). */
	titleBarInset: boolean;
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
		readsCalendar: READS_CALENDAR[os],
		titleBarInset: TITLE_BAR_INSET[os],
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

/**
 * The provided platform. Every page renders under `PlatformProvider`
 * (`App`, and `renderWithBridge` in the tests), so a component outside one
 * is a bug that throws here rather than guessing the Mac.
 */
export function usePlatform(): Platform {
	const provided = useContext(PlatformContext);
	if (!provided) {
		throw new Error("usePlatform needs a PlatformProvider above it");
	}
	return provided;
}

/**
 * The open popups Base UI renders: dialogs and popovers ("dialog"),
 * alert dialogs, menus and select lists.
 */
const POPUP =
	'[role="dialog"], [role="alertdialog"], [role="menu"], [role="listbox"]';

/** Whether a key event's target sits inside an open popup (`POPUP`). */
function inPopup(target: EventTarget | null): boolean {
	return target instanceof Element && target.closest(POPUP) !== null;
}

/**
 * Runs `action` when `shortcut` is pressed anywhere in the window, while
 * `enabled`; the key press goes no further. A press another handler
 * already took, or one inside an open dialog, popover or menu, is left
 * alone. The latest `action` runs, so a caller may pass a fresh closure
 * on every render.
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
			if (
				!event.repeat &&
				!event.defaultPrevented &&
				!inPopup(event.target) &&
				platform.matches(event, shortcut)
			) {
				event.preventDefault();
				latest.current();
			}
		};
		window.addEventListener("keydown", onKeyDown);
		return () => window.removeEventListener("keydown", onKeyDown);
	}, [platform, shortcut, enabled]);
}
