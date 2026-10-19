import { fireEvent, render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import {
	detectPlatform,
	keyHome,
	matchesShortcut,
	PlatformProvider,
	platformFor,
	SECRETS_FILE_WORDS,
	SHORTCUTS,
	type ShortcutEvent,
	shortcutLabel,
	useShortcut,
} from "./platform";

/** A key event; `code` is the US layout's for an ASCII letter unless given. */
function key(
	key: string,
	modifiers: Partial<Omit<ShortcutEvent, "key">> = {},
): ShortcutEvent {
	return {
		key,
		code: /^[a-z]$/i.test(key) ? `Key${key.toUpperCase()}` : "",
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
	});

	it("spells Ctrl, then Shift, on Windows and Linux", () => {
		for (const os of ["windows", "linux"] as const) {
			expect(shortcutLabel(os, SHORTCUTS.findMeetings)).toBe("Ctrl+F");
			expect(shortcutLabel(os, SHORTCUTS.exportAgain)).toBe("Ctrl+Shift+E");
			expect(shortcutLabel(os, SHORTCUTS.record)).toBe("Ctrl+Shift+R");
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

	it("takes the physical key where the layout types no ASCII character", () => {
		const find = SHORTCUTS.findMeetings;
		// Russian: the F key types "а"; Greek: "φ".
		expect(
			matchesShortcut(
				"windows",
				key("а", { code: "KeyF", ctrlKey: true }),
				find,
			),
		).toBe(true);
		expect(
			matchesShortcut("macos", key("φ", { code: "KeyF", metaKey: true }), find),
		).toBe(true);
		expect(
			matchesShortcut("linux", key("а", { code: "KeyA", ctrlKey: true }), find),
		).toBe(false);
		// A Latin layout keeps its own letters: Dvorak's "f" is on the Y key.
		expect(
			matchesShortcut("linux", key("f", { code: "KeyY", ctrlKey: true }), find),
		).toBe(true);
		expect(
			matchesShortcut("linux", key("y", { code: "KeyF", ctrlKey: true }), find),
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
});

describe("keyHome", () => {
	it("name the keyring on Linux only when the host keeps the key there", () => {
		const file = { short: SECRETS_FILE_WORDS, full: SECRETS_FILE_WORDS };
		expect(keyHome(platformFor("linux"), "keyring")).toEqual({
			short: "your keyring",
			full: "your keyring",
		});
		expect(keyHome(platformFor("linux"), "file")).toEqual(file);
		expect(keyHome(platformFor("linux"), undefined)).toEqual(file);
		expect(keyHome(platformFor("macos"), undefined)).toEqual({
			short: "your keychain",
			full: "your login keychain",
		});
		expect(keyHome(platformFor("windows"), "keyring")).toEqual({
			short: "Credential Manager",
			full: "Windows Credential Manager",
		});
	});
});

describe("useShortcut", () => {
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

	it("leave a press alone that a handler took or that lands in a popup", () => {
		const action = vi.fn();
		function Probe() {
			useShortcut(SHORTCUTS.findMeetings, action);
			return (
				<>
					<input data-testid="taken" onKeyDown={(e) => e.preventDefault()} />
					<div role="dialog">
						<input data-testid="in-dialog" />
					</div>
					<div role="menu">
						<button data-testid="in-menu" type="button" />
					</div>
					<input data-testid="page" />
				</>
			);
		}
		const { getByTestId } = render(
			<PlatformProvider platform={platformFor("linux")}>
				<Probe />
			</PlatformProvider>,
		);
		for (const id of ["taken", "in-dialog", "in-menu"]) {
			fireEvent.keyDown(getByTestId(id), { key: "f", ctrlKey: true });
		}
		expect(action).not.toHaveBeenCalled();
		fireEvent.keyDown(getByTestId("page"), { key: "f", ctrlKey: true });
		expect(action).toHaveBeenCalledTimes(1);
	});
});
