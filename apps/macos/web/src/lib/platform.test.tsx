import { fireEvent, render, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { platform as platformSchema } from "@/bridge/contract";
import {
	detectPlatform,
	matchesShortcut,
	PlatformProvider,
	platformFor,
	SHORTCUTS,
	type ShortcutEvent,
	shortcutLabel,
	usePlatform,
	useShortcut,
} from "./platform";

function key(
	key: string,
	modifiers: Partial<Omit<ShortcutEvent, "key">> = {},
): ShortcutEvent {
	return {
		key,
		metaKey: false,
		ctrlKey: false,
		shiftKey: false,
		altKey: false,
		...modifiers,
	};
}

describe("shortcutLabel", () => {
	it("writes the Mac's glyphs in the order the shortcut names them", () => {
		expect(shortcutLabel("macos", SHORTCUTS.findMeetings)).toBe("⌘F");
		expect(shortcutLabel("macos", SHORTCUTS.exportAgain)).toBe("⇧⌘E");
		expect(shortcutLabel("macos", SHORTCUTS.record)).toBe("⌘⇧R");
		expect(
			shortcutLabel("macos", { modifiers: ["alt", "mod"], key: "," }),
		).toBe("⌥⌘,");
	});

	it("spells Ctrl, Alt and Shift in that order on Windows and Linux", () => {
		for (const os of ["windows", "linux"] as const) {
			expect(shortcutLabel(os, SHORTCUTS.findMeetings)).toBe("Ctrl+F");
			expect(shortcutLabel(os, SHORTCUTS.exportAgain)).toBe("Ctrl+Shift+E");
			expect(shortcutLabel(os, SHORTCUTS.record)).toBe("Ctrl+Shift+R");
			expect(
				shortcutLabel(os, { modifiers: ["shift", "alt", "mod"], key: "P" }),
			).toBe("Ctrl+Alt+Shift+P");
		}
	});
});

describe("matchesShortcut", () => {
	it("takes ⌘ for mod on the Mac and Ctrl elsewhere", () => {
		const find = SHORTCUTS.findMeetings;
		expect(matchesShortcut("macos", key("f", { metaKey: true }), find)).toBe(
			true,
		);
		expect(matchesShortcut("macos", key("f", { ctrlKey: true }), find)).toBe(
			false,
		);
		expect(matchesShortcut("linux", key("f", { ctrlKey: true }), find)).toBe(
			true,
		);
		expect(matchesShortcut("windows", key("f", { metaKey: true }), find)).toBe(
			false,
		);
		expect(
			matchesShortcut(
				"windows",
				key("f", { ctrlKey: true, metaKey: true }),
				find,
			),
		).toBe(false);
	});

	it("wants exactly the shortcut's modifiers, the key in either case", () => {
		const exportAgain = SHORTCUTS.exportAgain;
		expect(
			matchesShortcut(
				"linux",
				key("E", { ctrlKey: true, shiftKey: true }),
				exportAgain,
			),
		).toBe(true);
		expect(
			matchesShortcut("linux", key("e", { ctrlKey: true }), exportAgain),
		).toBe(false);
		expect(
			matchesShortcut(
				"linux",
				key("E", { ctrlKey: true, shiftKey: true, altKey: true }),
				exportAgain,
			),
		).toBe(false);
		expect(
			matchesShortcut(
				"linux",
				key("f", { ctrlKey: true, shiftKey: true }),
				SHORTCUTS.findMeetings,
			),
		).toBe(false);
	});
});

describe("detectPlatform", () => {
	it("takes the shell's value first, then the flag, else the Swift app's Mac", () => {
		const shell = { __STENO_PLATFORM__: "linux" } as unknown as Window;
		expect(detectPlatform(shell, "windows")).toMatchObject({
			os: "linux",
			bindsRecordShortcut: true,
		});
		const bare = {} as Window;
		expect(detectPlatform(bare, "windows")).toMatchObject({
			os: "windows",
			bindsRecordShortcut: true,
		});
		expect(detectPlatform(bare)).toMatchObject({
			os: "macos",
			bindsRecordShortcut: false,
		});
	});

	it("ignores a value that is not a platform", () => {
		const odd = { __STENO_PLATFORM__: "beos" } as unknown as Window;
		expect(detectPlatform(odd, "amiga").os).toBe("macos");
	});

	it("knows every platform the contract names", () => {
		for (const os of platformSchema.options) {
			const platform = platformFor(os);
			expect(platform.words.computer).not.toBe("");
			expect(platform.permissions[0]).toBe("microphone");
		}
	});
});

describe("words", () => {
	it("keep the Mac's and say computer and the file manager elsewhere", () => {
		expect(platformFor("macos").words).toMatchObject({
			computer: "Mac",
			showInFileManager: "Show in Finder",
			systemSettings: "System Settings",
		});
		expect(platformFor("windows").words).toMatchObject({
			computer: "computer",
			showInFileManager: "Show in File Explorer",
			systemSettings: "Settings",
			opensSystemSettings: true,
		});
		expect(platformFor("linux").words).toMatchObject({
			computer: "computer",
			showInFileManager: "Show in folder",
			opensSystemSettings: false,
		});
		expect(platformFor("linux").words.localNetworkPrompt).toBeUndefined();
	});

	it("list the calendar on the Mac only", () => {
		expect(platformFor("macos").permissions).toContain("calendar");
		expect(platformFor("windows").permissions).not.toContain("calendar");
		expect(platformFor("linux").permissions).toEqual(["microphone"]);
	});
});

describe("usePlatform and useShortcut", () => {
	it("read the provided platform", () => {
		const { result } = renderHook(() => usePlatform(), {
			wrapper: ({ children }) => (
				<PlatformProvider platform={platformFor("windows")}>
					{children}
				</PlatformProvider>
			),
		});
		expect(result.current.os).toBe("windows");
		expect(result.current.label(SHORTCUTS.findMeetings)).toBe("Ctrl+F");
	});

	it("run the action for the platform's keys and stop the key there", () => {
		const action = vi.fn();
		function Probe({ enabled }: { enabled: boolean }) {
			useShortcut(SHORTCUTS.findMeetings, action, enabled);
			return null;
		}
		const { rerender } = render(
			<PlatformProvider platform={platformFor("linux")}>
				<Probe enabled />
			</PlatformProvider>,
		);
		expect(fireEvent.keyDown(window, { key: "f", metaKey: true })).toBe(true);
		expect(action).not.toHaveBeenCalled();
		expect(fireEvent.keyDown(window, { key: "f", ctrlKey: true })).toBe(false);
		expect(action).toHaveBeenCalledTimes(1);
		fireEvent.keyDown(window, { key: "f", ctrlKey: true, repeat: true });
		expect(action).toHaveBeenCalledTimes(1);
		rerender(
			<PlatformProvider platform={platformFor("linux")}>
				<Probe enabled={false} />
			</PlatformProvider>,
		);
		fireEvent.keyDown(window, { key: "f", ctrlKey: true });
		expect(action).toHaveBeenCalledTimes(1);
	});
});
