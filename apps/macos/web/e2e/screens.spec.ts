import { mkdirSync } from "node:fs";
import { expect, type Locator, type Page, test } from "@playwright/test";

/**
 * Renders the main window in every state at both reference window sizes,
 * the Settings window's sections and states at its 960 by 640 default size
 * (and at 200 % text and narrow, its sections in a drawer),
 * the onboarding window at its fixed 560 by 620 and the stories, each in
 * light and dark, to screens/, the speakers popover in each naming state
 * and the main window's column dividers. Every page must make zero requests
 * to anything but the preview server. `scenario=` and `tab=` bend the
 * fixtures (`src/bridge/mock-transport.ts`).
 */

const OUT = "screens";
const SCHEMES = ["light", "dark"] as const;
type Size = { width: number; height: number };

/** A state: its name, the query that opens it, the test id that must show. */
type State = readonly [name: string, query: string, ready: string];

const MAIN_SIZES: readonly Size[] = [
	{ width: 960, height: 600 },
	{ width: 1200, height: 760 },
];
const MAIN_STATES: readonly State[] = [
	["summary", "tab=summary", "tab-summary"],
	["transcript-picker", "tab=transcript&picker", "role=option"],
	["tasks", "tab=tasks", "tab-content-tasks"],
	["notes", "tab=notes", "scratchpad-editor"],
	["menu", "tab=summary&menu", "role=menu"],
	["empty", "scenario=empty", "empty-detail-title"],
	["recording", "scenario=recording", "auto-stop"],
	["denied", "scenario=denied", "denied-microphone"],
	["failed", "scenario=failed", "processing-failed"],
	["processing", "scenario=processing", "processing-card"],
	// The footer sits below the fold; the shot scrolls it into view.
	["export-failed", "scenario=export-failed&tab=summary", "export-again"],
];

const SETTINGS_SIZE: Size = { width: 960, height: 640 };
const SETTINGS_STATES: readonly State[] = [
	["general", "section=general", "update-status"],
	[
		"general-error",
		"section=general&scenario=settings-error",
		"login-item-approval",
	],
	["acknowledgements", "section=general", "acknowledgements-dialog"],
	["recording", "section=recording", "retention-mode"],
	["transcription", "section=transcription", "asset-offlineDiarizer-progress"],
	[
		"transcription-failed",
		"section=transcription&scenario=download-failed",
		"asset-parakeetV3-retry",
	],
	["summaries", "section=summaries", "summaries-status"],
	[
		"summaries-connected",
		"section=summaries&scenario=summaries-connected",
		"test-connection",
	],
	[
		"summaries-failed",
		"section=summaries&scenario=summaries-failed",
		"test-result-details",
	],
	[
		"codex-consent",
		"section=summaries&scenario=codex-consent",
		"codex-confirm",
	],
	["codex", "section=summaries&scenario=codex", "codex-model"],
	["export", "section=export", "export-enabled"],
	["export-on", "section=export&scenario=export-on", "export-status"],
	["iphone", "section=iphone", "begin-pairing"],
	["pairing", "section=iphone&scenario=pairing", "pairing-card"],
	[
		"iphone-unavailable",
		"section=iphone&scenario=phone-unavailable",
		"phone-unavailable",
	],
	["iphone-waiting", "section=iphone&scenario=phone-waiting", "phone-waiting"],
];

const ONBOARDING_SIZE: Size = { width: 560, height: 620 };
const ONBOARDING_STATES: readonly State[] = [
	["page-1", "", "permission-systemAudio-request"],
	[
		"page-1-unknown",
		"scenario=onboarding-unknown",
		"permission-microphone-request",
	],
	["page-1-denied", "scenario=onboarding-denied", "permission-microphone-open"],
	["page-1-granted", "scenario=onboarding-granted", "onboarding-done"],
	[
		"page-1-handoff",
		"scenario=onboarding-handoff",
		"onboarding-models-download",
	],
	[
		"page-1-handoff-downloading",
		"scenario=onboarding-handoff-downloading",
		"onboarding-models-progress",
	],
	["page-2", "scenario=onboarding-setup-open", "onboarding-save-summaries"],
	["page-2-codex", "scenario=onboarding-codex", "codex-confirm"],
	["page-2-saved", "scenario=onboarding-setup", "onboarding-vault-validation"],
	[
		"page-2-vault-saved",
		"scenario=onboarding-vault-saved",
		"setup-vault-saved",
	],
];

/** Clicked before the shot: the trigger of a dialog state. */
const CLICKS: Record<string, string> = { acknowledgements: "acknowledgements" };
/** Scrolled into view before the shot. */
const SCROLLS: Record<string, string> = {
	"export-failed": "meeting-footer",
	summaries: "summaries-status",
	"summaries-connected": "summaries-status",
	"summaries-failed": "summaries-status",
};

mkdirSync(OUT, { recursive: true });

function watchExternalRequests(page: Page): string[] {
	const external: string[] = [];
	page.on("request", (request) => {
		const url = new URL(request.url());
		const local =
			url.protocol === "data:" ||
			url.protocol === "blob:" ||
			url.protocol === "about:" ||
			url.hostname === "localhost" ||
			url.hostname === "127.0.0.1";
		if (!local) {
			external.push(request.url());
		}
	});
	return external;
}

async function settle(page: Page) {
	await page.waitForSelector("[data-ready]");
	await page.evaluate(() => document.fonts.ready);
	await page.waitForFunction(
		() => document.querySelector("[data-starting-style]") === null,
	);
	await page.waitForTimeout(300);
}

/**
 * A row holding the keyboard focus in a menu or a select's list draws the
 * 2 px `--focus` line inside it (`focusInsetClass`), not only the fill.
 */
async function expectFocusLine(row: Locator) {
	const style = await row.evaluate((element) => {
		const computed = getComputedStyle(element);
		return {
			style: computed.outlineStyle,
			width: computed.outlineWidth,
			offset: computed.outlineOffset,
		};
	});
	expect(style).toEqual({ style: "solid", width: "2px", offset: "-2px" });
}

/**
 * The ancestors that clip (overflow other than visible) and cut into the
 * focus ring around `target`: the 2 px ring past the 2 px offset.
 */
async function clippedRing(target: Locator) {
	return target.evaluate((element) => {
		const ring = element.getBoundingClientRect();
		const reach = 4;
		const cut: string[] = [];
		for (
			let parent = element.parentElement;
			parent;
			parent = parent.parentElement
		) {
			const style = getComputedStyle(parent);
			if (style.overflowX === "visible" && style.overflowY === "visible") {
				continue;
			}
			const box = parent.getBoundingClientRect();
			if (
				ring.left - reach < box.left ||
				ring.right + reach > box.right ||
				ring.top - reach < box.top ||
				ring.bottom + reach > box.bottom
			) {
				cut.push(parent.className);
			}
		}
		return cut;
	});
}

function shoot(
	window: "main" | "settings" | "onboarding",
	size: Size,
	scheme: (typeof SCHEMES)[number],
	[name, query, ready]: State,
	file: string,
) {
	test(`${window} ${scheme} ${file}`, async ({ page }) => {
		const external = watchExternalRequests(page);
		await page.setViewportSize(size);
		const params = [query, scheme === "dark" ? "dark" : ""]
			.filter(Boolean)
			.join("&");
		await page.goto(`/#/${window}${params ? `?${params}` : ""}`);
		await settle(page);
		await expect(page.getByTestId(`${window}-window`)).toBeVisible();
		const click = CLICKS[name];
		if (click) {
			await page.getByTestId(click).click();
		}
		const target = ready.startsWith("role=")
			? page.getByRole(ready.slice(5) as "menu" | "option").first()
			: page.getByTestId(ready);
		await expect(target).toBeVisible();
		const scroll = SCROLLS[name];
		if (scroll) {
			await page.getByTestId(scroll).scrollIntoViewIfNeeded();
		}
		await settle(page);
		await page.screenshot({ path: `${OUT}/${window}-${scheme}-${file}.png` });
		expect(external).toEqual([]);
	});
}

for (const scheme of SCHEMES) {
	for (const size of MAIN_SIZES) {
		for (const state of MAIN_STATES) {
			shoot(
				"main",
				size,
				scheme,
				state,
				`${size.width}x${size.height}-${state[0]}`,
			);
		}
	}
	for (const state of SETTINGS_STATES) {
		shoot("settings", SETTINGS_SIZE, scheme, state, state[0]);
	}
	for (const state of ONBOARDING_STATES) {
		shoot("onboarding", ONBOARDING_SIZE, scheme, state, state[0]);
	}

	test(`stories ${scheme}`, async ({ page }) => {
		const external = watchExternalRequests(page);
		await page.setViewportSize({ width: 1200, height: 760 });
		await page.goto(`/#/stories${scheme === "dark" ? "?dark" : ""}`);
		await settle(page);
		await expect(page.getByTestId("stories")).toBeVisible();
		// Popups flip to stay inside the viewport, so the whole page has to be
		// on screen: grow the viewport to the document instead of stitching.
		const height = await page.evaluate(
			() => document.documentElement.scrollHeight,
		);
		await page.setViewportSize({ width: 1200, height: Math.min(height, 8000) });
		await settle(page);
		await page.screenshot({ path: `${OUT}/stories-${scheme}.png` });
		expect(external).toEqual([]);
	});
}

/** A naming state: its name, the query, and what to do before the shot. */
type SpeakersState = readonly [
	name: string,
	query: string,
	act: (page: Page) => Promise<void>,
	size?: Size,
];

const openSpeakers = async (page: Page) => {
	await page.getByTestId("speakers-trigger").click();
	await expect(page.getByTestId("speakers-panel")).toBeVisible();
};

/** Opens the panel and the field on the first unnamed voice. */
const nameFirst = async (page: Page) => {
	await openSpeakers(page);
	await page
		.getByTestId("speakers-section-need")
		.getByRole("group")
		.first()
		.click();
	await expect(page.getByRole("combobox")).toBeFocused();
};

/** Picks the highlighted option for "Ann" (Anna, the voice match). */
const pickAnn = async (page: Page) => {
	await nameFirst(page);
	await page.keyboard.type("Ann");
	await expect(page.getByRole("option", { name: /Anna/ })).toBeVisible();
	await page.keyboard.press("Enter");
};

const transcriptName = (page: Page, name: string) =>
	page
		.getByTestId("tab-content-transcript")
		.getByRole("button", { name: new RegExp(`^${name}`) })
		.first();

const SPEAKERS_STATES: readonly SpeakersState[] = [
	["none-closed", "scenario=speakers-none", async () => {}],
	["none", "scenario=speakers-none", openSpeakers],
	// Every voice leads a paragraph, so each row shows its line and play.
	["partly-closed", "scenario=speakers-partly", async () => {}],
	["partly", "scenario=speakers-partly", openSpeakers],
	["check-closed", "scenario=speakers-check", async () => {}],
	["check", "scenario=speakers-check", openSpeakers],
	["done-closed", "scenario=speakers-done", async () => {}],
	["done", "scenario=speakers-done", openSpeakers],
	[
		"finding",
		"scenario=speakers-finding",
		async (page) => {
			await expect(page.getByTestId("speakers-finding")).toBeDisabled();
		},
	],
	["me-unnamed", "scenario=speakers-me-unnamed", openSpeakers],
	// Me named: "Nicolai · You" among the people, without a play.
	["me-named", "scenario=speakers-me", openSpeakers],
	[
		"keyboard",
		"scenario=speakers-partly",
		async (page) => {
			await page.getByTestId("speakers-trigger").focus();
			await page.keyboard.press("Enter");
			await expect(page.getByTestId("speakers-panel")).toBeVisible();
			await page.keyboard.press("ArrowDown");
		},
	],
	[
		// Keyboard focus on the voice match: its Y, N and P keys show.
		"keyboard-hints",
		"scenario=speakers-partly",
		async (page) => {
			await page.getByTestId("speakers-trigger").focus();
			await page.keyboard.press("Enter");
			await expect(page.getByTestId("speakers-panel")).toBeVisible();
			await expect(
				page.getByTestId("speakers-panel").locator("kbd").first(),
			).toBeVisible();
		},
	],
	[
		// Confirmed with Y, the row stays in place, done, and opens again.
		"reopened",
		"scenario=speakers-partly",
		async (page) => {
			await page.getByTestId("speakers-trigger").focus();
			await page.keyboard.press("Enter");
			await expect(page.getByTestId("speakers-panel")).toBeVisible();
			await page.keyboard.press("y");
			await expect(
				page.getByTestId("speakers-section-check").getByLabel("Named"),
			).toBeVisible();
			await page.keyboard.press("Enter");
			await expect(page.getByRole("option").first()).toBeVisible();
		},
	],
	[
		"editing",
		"scenario=speakers-none",
		async (page) => {
			await nameFirst(page);
			await expect(page.getByRole("option").first()).toBeVisible();
		},
	],
	[
		"typing",
		"scenario=speakers-none",
		async (page) => {
			await nameFirst(page);
			await page.keyboard.type("Ann");
			await expect(page.getByRole("option", { name: /Create/ })).toBeVisible();
			// The keys' highlight draws the menu rows' inset focus line.
			await expectFocusLine(page.locator('[role="option"][data-highlighted]'));
		},
	],
	[
		"searching",
		"scenario=speakers-options-slow",
		async (page) => {
			await nameFirst(page);
			await page.keyboard.type("Ann");
			await expect(page.getByTestId("speaker-options-searching")).toBeVisible();
		},
	],
	[
		"options-failed",
		"scenario=speakers-options-failed",
		async (page) => {
			await nameFirst(page);
			await page.keyboard.type("Ann");
			await expect(page.getByTestId("speaker-options-failed")).toBeVisible();
			await expect(
				page.getByRole("option", { name: /Create/ }),
			).toHaveAttribute("aria-selected", "true");
		},
	],
	[
		"saving",
		"scenario=speakers-saving",
		async (page) => {
			await pickAnn(page);
			await expect(page.getByText("Saving")).toBeVisible();
		},
	],
	[
		"failed",
		"scenario=speakers-select-failed",
		async (page) => {
			await pickAnn(page);
			await expect(page.getByRole("alert")).toContainText("database is locked");
			await expect(page.getByRole("combobox")).toHaveValue("Ann");
		},
	],
	[
		"advanced",
		"scenario=speakers-none",
		async (page) => {
			await pickAnn(page);
			await expect(page.getByRole("combobox")).toHaveAttribute(
				"aria-label",
				"Name for Speaker 2",
			);
		},
	],
	["split", "scenario=speakers-split", openSpeakers],
	[
		// Same person combines at once; the toast offers Undo, and the voice
		// nests under Nicolai's with Separate again.
		"combine-undo",
		"scenario=speakers-split",
		async (page) => {
			await openSpeakers(page);
			await page.getByRole("button", { name: "Same person" }).click();
			await expect(page.getByTestId("toast")).toContainText(
				"Speaker 4 combined with Nicolai.",
			);
			await expect(
				page.getByTestId("toast").getByRole("button", { name: "Undo" }),
			).toBeVisible();
		},
	],
	[
		// Speaker 5, combined into Nicolai's voice before this visit.
		"separate-again",
		"scenario=speakers-partly",
		async (page) => {
			await openSpeakers(page);
			const combined = page.getByTestId(/^speakers-popover-combined-/);
			await combined.scrollIntoViewIfNeeded();
			await expect(
				combined.getByRole("button", { name: /^Separate again/ }),
			).toBeVisible();
		},
	],
	[
		"separate-again-failed",
		"scenario=speakers-separate-failed",
		async (page) => {
			await openSpeakers(page);
			await page.getByRole("button", { name: /^Separate again/ }).click();
			const failure = page.getByTestId("speakers-panel").getByRole("alert");
			await expect(failure).toContainText("Nothing to separate.");
			await failure.scrollIntoViewIfNeeded();
		},
	],
	[
		"transcript",
		"scenario=speakers-partly&tab=transcript",
		async (page) => {
			await expect(
				page.getByTestId(/^speaker-suggested-/).first(),
			).toBeVisible();
		},
	],
	[
		"transcript-tooltip",
		"scenario=speakers-partly&tab=transcript",
		async (page) => {
			await transcriptName(page, "Anna").hover();
			await expect(
				page.getByText("Sounds like Anna · strong. Click to confirm or change"),
			).toBeVisible();
		},
	],
	[
		"transcript-panel",
		"scenario=speakers-partly&tab=transcript",
		async (page) => {
			await transcriptName(page, "Anna").click();
			await expect(page.getByRole("option").first()).toBeVisible();
		},
	],
	[
		"transcript-unnamed",
		"scenario=speakers-partly&tab=transcript",
		async (page) => {
			await transcriptName(page, "Speaker 4").click();
			await expect(page.getByRole("option").first()).toBeVisible();
		},
	],
	[
		// An unnamed voice opens on All.
		"transcript-scope-all",
		"scenario=speakers-partly&tab=transcript",
		async (page) => {
			await transcriptName(page, "Speaker 4").click();
			await expect(
				page.getByRole("radio", { name: "All of Speaker 4" }),
			).toBeChecked();
			await expect(page.getByRole("option").first()).toBeVisible();
		},
	],
	[
		// A confirmed one opens on This paragraph, its scope line saying so.
		"transcript-scope-paragraph",
		"scenario=speakers-partly&tab=transcript",
		async (page) => {
			await transcriptName(page, "Jérôme").click();
			await expect(
				page.getByRole("radio", { name: "This paragraph" }),
			).toBeChecked();
			await expect(page.getByRole("option").first()).toBeVisible();
		},
	],
	[
		// After a move: the toast, and the paragraph under its new voice.
		"transcript-moved",
		"scenario=speakers-partly&tab=transcript",
		async (page) => {
			await transcriptName(page, "Jérôme").click();
			await page.keyboard.type("Lea");
			await expect(page.getByRole("option", { name: /Create/ })).toBeVisible();
			await page.keyboard.press("Enter");
			await expect(page.getByTestId("toast")).toContainText(
				"This paragraph is now Lea’s.",
			);
		},
	],
	[
		"transcript-stale",
		"scenario=speakers-move-stale&tab=transcript",
		async (page) => {
			await transcriptName(page, "Jérôme").click();
			await page.keyboard.type("Ann");
			await expect(page.getByRole("option", { name: /Create/ })).toBeVisible();
			await page.keyboard.press("Enter");
			await expect(page.getByTestId("speaker-panel-stale")).toBeVisible();
			await expect(page.getByRole("combobox")).toHaveValue("Ann");
		},
	],
	[
		"transcript-no-voice",
		"scenario=speakers-no-voice&tab=transcript",
		async (page) => {
			await transcriptName(page, "Unknown").click();
			await expect(page.getByRole("option").first()).toBeVisible();
			await expect(page.getByRole("radiogroup")).toHaveCount(0);
		},
	],
	[
		"transcript-playing",
		"scenario=speakers-partly&tab=transcript",
		async (page) => {
			await transcriptName(page, "Nicolai").click();
			await page.getByRole("button", { name: "Play this paragraph" }).click();
			await expect(
				page.getByRole("button", { name: "Stop this paragraph" }),
			).toBeVisible();
		},
	],
	[
		"transcript-no-recording",
		"scenario=speakers-no-recording&tab=transcript",
		async (page) => {
			await transcriptName(page, "Nicolai").click();
			await expect(page.getByText("No recording")).toBeVisible();
			await expect(page.getByRole("option").first()).toBeVisible();
		},
	],
	// The voice match's strength in words: "Sounds like Anna · likely".
	["likely", "scenario=speakers-likely", openSpeakers],
	[
		// N on the voice match: the suggestion goes, the voice moves to Need
		// names and its field opens without Anna.
		"rejected",
		"scenario=speakers-partly",
		async (page) => {
			await page.getByTestId("speakers-trigger").focus();
			await page.keyboard.press("Enter");
			await expect(page.getByTestId("speakers-panel")).toBeVisible();
			await page.keyboard.press("n");
			await expect(page.getByRole("combobox")).toBeFocused();
			await expect(page.getByRole("option").first()).toBeVisible();
		},
	],
	[
		// After No, typing her name offers Anna again.
		"rejected-typed",
		"scenario=speakers-partly",
		async (page) => {
			await page.getByTestId("speakers-trigger").focus();
			await page.keyboard.press("Enter");
			await expect(page.getByTestId("speakers-panel")).toBeVisible();
			await page.keyboard.press("n");
			await expect(page.getByRole("combobox")).toBeFocused();
			await page.keyboard.type("Ann");
			await expect(page.getByRole("option", { name: /Anna/ })).toBeVisible();
		},
	],
	[
		// Not a person from the field: the row keeps its place, says so,
		// and offers Restore.
		"dismissing",
		"scenario=speakers-none",
		async (page) => {
			await nameFirst(page);
			await page.getByTestId("speaker-action-dismiss").click();
			await expect(
				page.getByRole("button", { name: /^Restore/ }),
			).toBeVisible();
		},
	],
	["dismissed-closed", "scenario=speakers-dismissed", async () => {}],
	["dismissed", "scenario=speakers-dismissed", openSpeakers],
	// No voice is a person: the pill is done, "No speakers".
	["not-people-closed", "scenario=speakers-not-people", async () => {}],
	["not-people", "scenario=speakers-not-people", openSpeakers],
	// No voice's words are exactly its clip: nearby words, unquoted,
	// "Around the sample:" where a clip plays, bare beside "No sample".
	["context-quote", "scenario=speakers-context", openSpeakers],
	[
		"transcript-dismissed",
		"scenario=speakers-dismissed&tab=transcript",
		async (page) => {
			await expect(
				page.getByTestId("tab-content-transcript").getByText("Not a person"),
			).not.toHaveCount(0);
		},
	],
	[
		// A confirmed person's field ends with Remove name.
		"remove-name",
		"scenario=speakers-done",
		async (page) => {
			await openSpeakers(page);
			await page
				.getByTestId("speakers-section-people")
				.getByRole("group")
				.first()
				.click();
			await expect(page.getByTestId("speaker-action-clear")).toBeVisible();
		},
	],
	[
		// The one voice left and the one invitee nobody is named as.
		"last-invitee",
		"scenario=speakers-last-invitee",
		async (page) => {
			await openSpeakers(page);
			await page
				.getByTestId("speakers-section-need")
				.getByRole("group")
				.first()
				.click();
			await expect(page.getByText("Last invitee left")).toBeVisible();
		},
	],
	[
		"transcript-no",
		"scenario=speakers-partly&tab=transcript",
		async (page) => {
			await transcriptName(page, "Anna").click();
			await page.getByRole("button", { name: "No", exact: true }).click();
			await expect(page.getByRole("option").first()).toBeVisible();
		},
	],
	["long", "scenario=speakers-long", openSpeakers],
	[
		"long-transcript-960",
		"scenario=speakers-long&tab=transcript",
		async (page) => {
			// The long name truncates; the turn's time stays on its line.
			const turn = page.getByTestId(/^turn-/).nth(1);
			await expect(turn.locator(".font-mono")).toBeInViewport({ ratio: 1 });
			await expect(turn.getByRole("button")).toHaveAttribute(
				"aria-label",
				/Hohenzollern-Sigmaringen$/,
			);
		},
		{ width: 960, height: 600 },
	],
	[
		"long-960",
		"scenario=speakers-long",
		openSpeakers,
		{ width: 960, height: 600 },
	],
	// 200 % zoom: the 1200 by 760 window as a 600 by 380 viewport.
	["zoom-200", "", openSpeakers, { width: 600, height: 380 }],
	[
		"zoom-200-editing",
		"scenario=speakers-none",
		nameFirst,
		{ width: 600, height: 380 },
	],
	[
		"zoom-200-transcript",
		"scenario=speakers-partly&tab=transcript",
		async (page) => {
			await transcriptName(page, "Speaker 4").click();
			await expect(page.getByRole("option").first()).toBeVisible();
		},
		{ width: 600, height: 380 },
	],
	// Below 18 rem the pill shows the count alone and opens the same panel.
	["narrow-count", "", openSpeakers, { width: 320, height: 640 }],
];

/** The speakers pill and panel, at 1200 by 760 unless said, in each naming state. */
for (const scheme of SCHEMES) {
	for (const [name, query, act, size] of SPEAKERS_STATES) {
		test(`main ${scheme} speakers-${name}`, async ({ page }) => {
			const external = watchExternalRequests(page);
			const viewport = size ?? { width: 1200, height: 760 };
			await page.setViewportSize(viewport);
			const params = [query, scheme === "dark" ? "dark" : ""]
				.filter(Boolean)
				.join("&");
			await page.goto(`/#/main${params ? `?${params}` : ""}`);
			await settle(page);
			await act(page);
			await settle(page);
			const panel = page.getByTestId("speakers-panel");
			if (await panel.isVisible()) {
				// The panel fits the viewport, height included.
				const box = await panel.boundingBox();
				expect((box?.y ?? 0) + (box?.height ?? 0)).toBeLessThanOrEqual(
					viewport.height,
				);
				expect((box?.x ?? 0) + (box?.width ?? 0)).toBeLessThanOrEqual(
					viewport.width,
				);
			}
			if (name === "zoom-200-editing" || name === "zoom-200-transcript") {
				// Cut short at 200 %, the panel says it scrolls: its end fades.
				await expect(
					page.locator('[data-testid="popup-scroll"][data-overflow-y-end]'),
				).toBeVisible();
			}
			await page.screenshot({
				path: `${OUT}/main-${scheme}-speakers-${name}.png`,
			});
			expect(external).toEqual([]);
		});
	}
}

/**
 * The column dividers at 1200 by 760: the sidebar's under the pointer, then
 * mid-drag, both columns dragged wider, and the same widths after a reload
 * (they are kept in `localStorage`).
 */
for (const scheme of SCHEMES) {
	test(`main ${scheme} columns`, async ({ page }) => {
		const external = watchExternalRequests(page);
		await page.setViewportSize({ width: 1200, height: 760 });
		await page.goto(`/#/main${scheme === "dark" ? "?dark" : ""}`);
		await settle(page);
		const shot = (name: string) =>
			page.screenshot({ path: `${OUT}/main-${scheme}-columns-${name}.png` });
		const drag = async (testID: string, dx: number) => {
			const box = await page.getByTestId(testID).boundingBox();
			if (!box) {
				throw new Error(`${testID} has no box`);
			}
			const x = box.x + box.width / 2;
			await page.mouse.move(x, 300);
			await page.mouse.down();
			await page.mouse.move(x + dx, 300, { steps: 8 });
		};

		const divider = await page.getByTestId("resize-sidebar").boundingBox();
		if (!divider) {
			throw new Error("resize-sidebar has no box");
		}
		const start = divider.x + divider.width / 2;
		await page.mouse.move(start, 300);
		await page.waitForTimeout(300);
		await shot("hover");
		await page.mouse.down();
		await page.mouse.move(start + 64, 300, { steps: 8 });
		await shot("dragging");
		await page.mouse.up();
		await drag("resize-list", 80);
		await page.mouse.up();
		await page.mouse.move(1100, 700);
		await expect(page.getByTestId("resize-sidebar")).toHaveAttribute(
			"aria-valuenow",
			"320",
		);
		await expect(page.getByTestId("resize-list")).toHaveAttribute(
			"aria-valuenow",
			"380",
		);
		await page.waitForTimeout(300);
		await shot("dragged");

		await page.reload();
		await settle(page);
		await expect(page.getByTestId("main-window")).toHaveCSS(
			"grid-template-columns",
			/^320px 380px /,
		);
		await shot("after-reload");
		expect(external).toEqual([]);
	});
}

/**
 * The keyboard walk: Tab through the main window from the page's start and
 * shoot each kind of control the first time it holds focus, clipped to the
 * control and its surroundings so the ring and its offset are legible.
 */
const FOCUS_STOPS: Record<string, string> = {
	"sidebar-record": "button-primary",
	"nav-all": "sidebar-row",
	"search-meetings": "field-search",
	"meeting-00000000-0000-0000-0000-000000000001": "list-row",
	"meeting-actions": "button-icon",
	"speakers-trigger": "header-pill",
	"tab-summary": "tab",
	"setup-summaries": "link",
	"banner-not-now": "link-not-now",
	checkbox: "checkbox",
	"keep-audio": "switch",
};

for (const scheme of SCHEMES) {
	test(`main ${scheme} keyboard walk`, async ({ page }) => {
		const external = watchExternalRequests(page);
		await page.setViewportSize({ width: 1200, height: 760 });
		await page.goto(`/#/main?tab=summary${scheme === "dark" ? "&dark" : ""}`);
		await settle(page);
		const seen = new Set<string>();
		for (let press = 0; press < 80 && seen.size < 12; press += 1) {
			await page.keyboard.press("Tab");
			const stop = await page.evaluate(() => {
				const active = document.activeElement;
				if (!(active instanceof HTMLElement)) {
					return null;
				}
				const box = active.getBoundingClientRect();
				return {
					key:
						active.getAttribute("role") === "checkbox"
							? "checkbox"
							: (active.closest("[data-testid]")?.getAttribute("data-testid") ??
								""),
					box: { x: box.x, y: box.y, width: box.width, height: box.height },
				};
			});
			const name = stop ? FOCUS_STOPS[stop.key] : undefined;
			if (!stop || !name || seen.has(name)) {
				continue;
			}
			seen.add(name);
			await page.waitForTimeout(200);
			const pad = 40;
			const x = Math.max(0, stop.box.x - pad);
			const y = Math.max(0, stop.box.y - pad);
			await page.screenshot({
				clip: {
					x,
					y,
					width: Math.min(1200 - x, stop.box.width + pad * 2),
					height: Math.min(760 - y, stop.box.height + pad * 2),
				},
				path: `${OUT}/main-${scheme}-focus-${name}.png`,
			});
		}
		expect([...seen].sort()).toEqual(Object.values(FOCUS_STOPS).sort());
		expect(external).toEqual([]);
	});

	/**
	 * 200 % text: the 1200 by 760 window at 200 % browser zoom, which is a
	 * 600 by 380 CSS-pixel viewport (the shot keeps the 2x device scale, so
	 * it is 1200 by 760 pixels). Zoom rather than a 200 % root font size,
	 * because the kit sizes type in rem but spacing and some controls in px,
	 * and zoom scales both, as WebKit's page zoom does. The window is narrow
	 * there: the reading column takes it, its header holds without
	 * overflowing, every tab can be reached, and the Meetings button opens
	 * the sidebar and list as a drawer.
	 */
	test(`main ${scheme} zoom-200`, async ({ page }) => {
		const external = watchExternalRequests(page);
		await page.setViewportSize({ width: 600, height: 380 });
		await page.goto(`/#/main?tab=summary${scheme === "dark" ? "&dark" : ""}`);
		await settle(page);
		await expect(page.getByTestId("main-window")).toBeVisible();
		await expect(page.getByTestId("meeting-list")).toHaveCount(0);
		await page.screenshot({ path: `${OUT}/main-${scheme}-zoom-200.png` });

		const header = page.locator("main header").first();
		const overflow = await header.evaluate(
			(element) => element.scrollWidth - element.clientWidth,
		);
		expect(overflow).toBeLessThanOrEqual(0);
		const exportBox = await page.getByTestId("export-meeting").boundingBox();
		expect(exportBox).not.toBeNull();
		expect((exportBox?.x ?? 0) + (exportBox?.width ?? 0)).toBeLessThanOrEqual(
			600,
		);
		const crumb = page.locator("main [aria-current=page]");
		expect((await crumb.boundingBox())?.width ?? 0).toBeGreaterThan(60);
		for (const tab of ["summary", "transcript", "tasks", "notes"]) {
			const target = page.getByTestId(`tab-${tab}`);
			await target.scrollIntoViewIfNeeded();
			await expect(target).toBeInViewport();
		}

		await page.getByTestId("show-meetings").click();
		await expect(page.getByTestId("meetings-drawer")).toBeVisible();
		await expect(page.getByTestId("meeting-list")).toBeVisible();
		await settle(page);
		await expect(page.getByTestId("close-meetings")).toBeInViewport({
			ratio: 1,
		});
		await page.screenshot({
			path: `${OUT}/main-${scheme}-zoom-200-meetings.png`,
		});
		// Close gives the focus back to the Meetings button; so does a pick.
		await page.getByTestId("close-meetings").click();
		await expect(page.getByTestId("meetings-drawer")).toHaveCount(0);
		await expect(page.getByTestId("show-meetings")).toBeFocused();
		await page.getByTestId("show-meetings").click();
		await page
			.getByTestId("meeting-00000000-0000-0000-0000-000000000003")
			.click();
		await expect(page.getByTestId("meetings-drawer")).toHaveCount(0);
		await expect(page.getByTestId("show-meetings")).toBeFocused();
		await expect(page.getByTestId("recorder-bar")).toHaveCount(0);
		expect(external).toEqual([]);
	});

	/**
	 * Recording in a narrow window (200 % text), another meeting open: the
	 * sidebar is in the closed drawer, so the recorder bar under the header
	 * row holds the status, Stop, the auto-stop countdown and Keep
	 * recording, all in view without opening the drawer.
	 */
	test(`main ${scheme} narrow recording`, async ({ page }) => {
		const external = watchExternalRequests(page);
		await page.setViewportSize({ width: 600, height: 380 });
		await page.goto(
			`/#/main?scenario=recording-elsewhere${scheme === "dark" ? "&dark" : ""}`,
		);
		await settle(page);
		await expect(page.getByTestId("meeting-list")).toHaveCount(0);
		await expect(page.getByTestId("header-live")).toHaveCount(0);
		const bar = page.getByTestId("recorder-bar");
		await expect(bar).toContainText("Recording");
		// The meeting that records is named, as a link to it.
		await expect(bar.getByTestId("bar-open-recording")).toHaveText(
			"Tuesday 10:08",
		);
		for (const id of [
			"bar-open-recording",
			"bar-stop",
			"auto-stop",
			"keep-recording",
		]) {
			await expect(bar.getByTestId(id)).toBeInViewport({ ratio: 1 });
		}
		await page.screenshot({
			path: `${OUT}/main-${scheme}-narrow-recording.png`,
		});
		expect(external).toEqual([]);
	});

	/**
	 * The recorder bar's title link from the keyboard: its ring (2 px past a
	 * 2 px offset) is drawn whole, no ancestor that clips holds it short.
	 */
	test(`main ${scheme} narrow recording focus title`, async ({ page }) => {
		await page.setViewportSize({ width: 600, height: 380 });
		await page.goto(
			`/#/main?scenario=recording-elsewhere${scheme === "dark" ? "&dark" : ""}`,
		);
		await settle(page);
		const link = page.getByTestId("bar-open-recording");
		await link.focus();
		await page.keyboard.press("Shift+Tab");
		await page.keyboard.press("Tab");
		await expect(link).toBeFocused();
		expect(await clippedRing(link)).toEqual([]);
		await page.screenshot({
			path: `${OUT}/main-${scheme}-narrow-recording-focus-title.png`,
		});
	});

	/**
	 * A long title in the recorder bar truncates inside its link; the middle
	 * dot, the elapsed time and Stop keep their place.
	 */
	test(`main ${scheme} narrow recording long title`, async ({ page }) => {
		await page.setViewportSize({ width: 600, height: 380 });
		const title = encodeURIComponent(
			"Quarterly planning with the platform, design and research teams",
		);
		await page.goto(
			`/#/main?scenario=recording-elsewhere&recordingTitle=${title}${scheme === "dark" ? "&dark" : ""}`,
		);
		await settle(page);
		const bar = page.getByTestId("recorder-bar");
		const text = bar.getByTestId("bar-open-recording").locator("span");
		expect(
			await text.evaluate(
				(element) => element.scrollWidth > element.clientWidth,
			),
		).toBe(true);
		await expect(bar).toContainText("·");
		const elapsed = bar.getByTestId("bar-elapsed");
		await expect(elapsed).toHaveText(/^12:3\d$/);
		await expect(elapsed).toBeInViewport({ ratio: 1 });
		const clock = await elapsed.boundingBox();
		const stop = await bar.getByTestId("bar-stop").boundingBox();
		expect((clock?.x ?? 0) + (clock?.width ?? 0)).toBeLessThanOrEqual(
			stop?.x ?? 0,
		);
		await page.screenshot({
			path: `${OUT}/main-${scheme}-narrow-recording-long-title.png`,
		});
	});

	/**
	 * A search that leaves the recording meeting out of the list: the bar
	 * still links to it, as "Open current recording" when the list never
	 * showed its title.
	 */
	test(`main ${scheme} narrow recording search`, async ({ page }) => {
		await page.setViewportSize({ width: 600, height: 380 });
		await page.goto(
			`/#/main?scenario=recording-elsewhere&query=investor${scheme === "dark" ? "&dark" : ""}`,
		);
		await settle(page);
		const link = page
			.getByTestId("recorder-bar")
			.getByTestId("bar-open-recording");
		await expect(link).toHaveText("Open current recording");
		await expect(link).toBeInViewport({ ratio: 1 });
		await page.screenshot({
			path: `${OUT}/main-${scheme}-narrow-recording-search.png`,
		});
	});

	/**
	 * The ⋯ menu opened from the keyboard: the first template choice holds
	 * the focus, its check in the leading slot, a separator before Re-run
	 * summary.
	 */
	test(`main ${scheme} focus menu radio`, async ({ page }) => {
		const external = watchExternalRequests(page);
		await page.setViewportSize({ width: 1200, height: 760 });
		await page.goto(`/#/main?tab=summary${scheme === "dark" ? "&dark" : ""}`);
		await settle(page);
		await page.getByTestId("meeting-actions").focus();
		await page.keyboard.press("Enter");
		const menu = page.getByRole("menu");
		await expect(menu).toBeVisible();
		const radio = menu.getByRole("menuitemradio").first();
		await expect(radio).toBeFocused();
		await expect(radio).toHaveAttribute("data-highlighted", "");
		await expectFocusLine(radio);
		await settle(page);
		const box = await menu.boundingBox();
		if (!box) {
			throw new Error("the menu has no box");
		}
		const pad = 24;
		await page.screenshot({
			clip: {
				x: box.x - pad,
				y: box.y - pad,
				width: box.width + pad * 2,
				height: Math.min(760 - box.y + pad, box.height + pad * 2),
			},
			path: `${OUT}/main-${scheme}-focus-menu-radio.png`,
		});
		expect(external).toEqual([]);
	});

	/**
	 * Keyboard focus on a plain ⋯ menu row and on a select's option: the
	 * inset focus line over the highlight, measured at 3:1 on the popover.
	 */
	test(`main ${scheme} focus menu item`, async ({ page }) => {
		const external = watchExternalRequests(page);
		await page.setViewportSize({ width: 1200, height: 760 });
		await page.goto(`/#/main?tab=summary${scheme === "dark" ? "&dark" : ""}`);
		await settle(page);
		await page.getByTestId("meeting-actions").focus();
		await page.keyboard.press("Enter");
		const menu = page.getByRole("menu");
		await expect(menu).toBeVisible();
		const item = menu.getByRole("menuitem", { name: "Copy transcript" });
		for (let step = 0; step < 12; step += 1) {
			if (
				await item.evaluate((element) => element === document.activeElement)
			) {
				break;
			}
			await page.keyboard.press("ArrowDown");
		}
		await expect(item).toBeFocused();
		await expectFocusLine(item);
		await settle(page);
		const box = await menu.boundingBox();
		if (!box) {
			throw new Error("the menu has no box");
		}
		const pad = 24;
		await page.screenshot({
			clip: {
				x: box.x - pad,
				y: box.y - pad,
				width: box.width + pad * 2,
				height: Math.min(760 - box.y + pad, box.height + pad * 2),
			},
			path: `${OUT}/main-${scheme}-focus-menu-item.png`,
		});
		expect(external).toEqual([]);
	});

	test(`settings ${scheme} focus select option`, async ({ page }) => {
		const external = watchExternalRequests(page);
		await page.setViewportSize(SETTINGS_SIZE);
		await page.goto(
			`/#/settings?section=recording${scheme === "dark" ? "&dark" : ""}`,
		);
		await settle(page);
		await page.getByTestId("retention-mode").focus();
		await page.keyboard.press("ArrowDown");
		const list = page.getByRole("listbox");
		await expect(list).toBeVisible();
		const option = list.getByRole("option").first();
		await page.keyboard.press("Home");
		await expect(option).toBeFocused();
		await expectFocusLine(option);
		await settle(page);
		const box = await list.boundingBox();
		if (!box) {
			throw new Error("the list has no box");
		}
		const pad = 24;
		await page.screenshot({
			clip: {
				x: box.x - pad,
				y: box.y - pad,
				width: box.width + pad * 2,
				height: box.height + pad * 2,
			},
			path: `${OUT}/settings-${scheme}-focus-select-option.png`,
		});
		expect(external).toEqual([]);
	});

	/**
	 * 960 by 600 with the default column widths: the first summary heading
	 * and the first transcript paragraph show without scrolling, and with
	 * the recorder held the page crumb keeps its whole title beside the
	 * live mark, which drops its word and keeps the time. (The trail is
	 * 165 px here, under the 20 rem below which "Meetings /" goes, idle or
	 * recording alike.)
	 */
	test(`main ${scheme} 960 reading`, async ({ page }) => {
		await page.setViewportSize({ width: 960, height: 600 });
		const dark = scheme === "dark" ? "&dark" : "";
		await page.goto(`/#/main?tab=summary${dark}`);
		await settle(page);
		const heading = page.getByTestId("tab-content-summary").locator("h3");
		await expect(heading.first()).toBeInViewport({ ratio: 1 });
		// Idle as recording: the trail is too narrow for "Meetings /", so the
		// page crumb has the row to itself.
		for (const parent of await page
			.locator("main nav[aria-label=Breadcrumb] li:not([aria-current])")
			.all()) {
			await expect(parent).toBeHidden();
		}
		await page.goto(`/#/main?tab=transcript${dark}`);
		await page.reload();
		await settle(page);
		await expect(
			page.getByTestId("tab-content-transcript").locator("p").first(),
		).toBeInViewport();
		await page.goto(`/#/main?scenario=recording${dark}`);
		await page.reload();
		await settle(page);
		await expect(page.getByTestId("header-live")).toBeVisible();
		// One Stop: the sidebar's; the header holds only the live mark.
		await expect(page.getByRole("button", { name: /^Stop/ })).toHaveCount(1);
		for (const parent of await page
			.locator("main nav[aria-label=Breadcrumb] li:not([aria-current])")
			.all()) {
			await expect(parent).toBeHidden();
		}
		const crumb = page.locator("main [aria-current=page]");
		expect((await crumb.boundingBox())?.width ?? 0).toBeGreaterThan(60);
		expect(
			await crumb.evaluate(
				(element) => element.scrollWidth > element.clientWidth,
			),
		).toBe(false);
		// The word is for a screen reader only here; the mark and time show.
		const live = page.getByTestId("header-live");
		await expect(live.getByText("Recording", { exact: true })).toHaveCSS(
			"width",
			"1px",
		);
		await expect(live).toContainText(/12:3\d/);
		await page.screenshot({ path: `${OUT}/main-${scheme}-960-recording.png` });
	});

	/**
	 * Transcription failed: the red line takes the empty tabs' place and the
	 * personal notes stay below it, editable.
	 */
	test(`main ${scheme} failed notes`, async ({ page }) => {
		const external = watchExternalRequests(page);
		await page.setViewportSize({ width: 1200, height: 760 });
		await page.goto(
			`/#/main?scenario=failed${scheme === "dark" ? "&dark" : ""}`,
		);
		await settle(page);
		await expect(page.getByTestId("processing-failed")).toBeVisible();
		await expect(page.getByRole("tablist")).toHaveCount(0);
		const notes = page.getByTestId("failed-notes").getByRole("textbox");
		await notes.fill("Ask Jérôme for the Q1 numbers again.");
		await page.locator("body").click({ position: { x: 900, y: 700 } });
		await settle(page);
		await page.screenshot({ path: `${OUT}/main-${scheme}-failed-notes.png` });
		expect(external).toEqual([]);
	});

	/**
	 * Only the summary failed: the Summary tab holds the red line with
	 * Re-run summary, and the transcript stays readable.
	 */
	test(`main ${scheme} summary failed`, async ({ page }) => {
		const external = watchExternalRequests(page);
		await page.setViewportSize({ width: 1200, height: 760 });
		const dark = scheme === "dark" ? "&dark" : "";
		await page.goto(`/#/main?scenario=summary-failed&tab=transcript${dark}`);
		await settle(page);
		await expect(
			page.getByTestId("tab-content-transcript").locator("p").first(),
		).toBeInViewport();
		await expect(page.getByTestId("processing-failed")).toHaveCount(0);
		await page.screenshot({
			path: `${OUT}/main-${scheme}-summary-failed-transcript.png`,
		});
		await page.getByTestId("tab-summary").click();
		await expect(page.getByTestId("failure-rerun-summary")).toBeVisible();
		await settle(page);
		await page.screenshot({
			path: `${OUT}/main-${scheme}-summary-failed-summary.png`,
		});
		expect(external).toEqual([]);
	});

	/**
	 * Onboarding's page 1 fits its 560 by 620 window: in the denied and the
	 * unknown state the last permission's action shows above the footer.
	 */
	test(`onboarding ${scheme} page 1 fits`, async ({ page }) => {
		await page.setViewportSize(ONBOARDING_SIZE);
		const dark = scheme === "dark" ? "&dark" : "";
		for (const scenario of ["onboarding-denied", "onboarding-unknown"]) {
			await page.goto(`/#/onboarding?scenario=${scenario}${dark}`);
			await page.reload();
			await settle(page);
			await expect(
				page.getByTestId("permission-localNetwork-skip"),
			).toBeInViewport({ ratio: 1 });
		}
	});

	/**
	 * Settings at 200 % text (its 960 by 640 window is 480 by 320 CSS px)
	 * and in a narrow 640 px window: the sections leave for a drawer behind
	 * the header's Sections button, the form takes the window, and nothing
	 * runs off its trailing edge, Open System Settings included.
	 */
	for (const [label, size] of [
		["zoom-200", { width: 480, height: 320 }],
		["narrow", { width: 640, height: 640 }],
	] as const) {
		test(`settings ${scheme} ${label}`, async ({ page }) => {
			const external = watchExternalRequests(page);
			await page.setViewportSize(size);
			const dark = scheme === "dark" ? "&dark" : "";
			for (const section of ["general", "iphone"]) {
				await page.goto(`/#/settings?section=${section}${dark}`);
				await page.reload();
				await settle(page);
				await expect(page.getByTestId("settings-window")).toHaveAttribute(
					"data-narrow",
				);
				await expect(page.getByTestId("settings-general")).toHaveCount(0);
				const overflow = await page.evaluate(() => {
					let worst = 0;
					for (const element of document.querySelectorAll(
						"[data-testid=settings-window] button",
					)) {
						const box = element.getBoundingClientRect();
						if (box.width > 0) {
							worst = Math.max(worst, box.right - window.innerWidth);
						}
					}
					return worst;
				});
				expect(overflow).toBeLessThanOrEqual(0);
				await page.screenshot({
					path: `${OUT}/settings-${scheme}-${label}-${section}.png`,
				});
			}
			const open = page.getByTestId("phone-local-network-open");
			if ((await open.count()) > 0) {
				await open.scrollIntoViewIfNeeded();
				await expect(open).toBeInViewport({ ratio: 1 });
				await settle(page);
				await page.screenshot({
					path: `${OUT}/settings-${scheme}-${label}-iphone-local-network.png`,
				});
			}
			await page.getByTestId("show-sections").click();
			const drawer = page.getByTestId("sections-drawer");
			await expect(drawer).toBeVisible();
			// Named in its header row, as the main window's drawer is.
			await expect(
				drawer.getByRole("heading", { name: "Sections" }),
			).toBeInViewport({ ratio: 1 });
			await settle(page);
			await page.screenshot({
				path: `${OUT}/settings-${scheme}-${label}-sections.png`,
			});
			await page.getByTestId("settings-recording").click();
			await expect(page.getByTestId("sections-drawer")).toHaveCount(0);
			await expect(page.getByTestId("show-sections")).toBeFocused();
			expect(external).toEqual([]);
		});
	}

	/** A dialog open with keyboard focus on its primary action. */
	test(`settings ${scheme} dialog`, async ({ page }) => {
		const external = watchExternalRequests(page);
		await page.setViewportSize(SETTINGS_SIZE);
		await page.goto(
			`/#/settings?section=general${scheme === "dark" ? "&dark" : ""}`,
		);
		await settle(page);
		await page.getByTestId("acknowledgements").click();
		const dialog = page.getByRole("dialog");
		await expect(dialog).toBeVisible();
		await dialog.getByRole("button", { name: "Done" }).focus();
		await page.keyboard.press("Shift+Tab");
		await page.keyboard.press("Tab");
		await settle(page);
		await page.screenshot({ path: `${OUT}/settings-${scheme}-dialog.png` });
		expect(external).toEqual([]);
	});
}

/**
 * Results are announced: each result line shown sits in a live region (a
 * `ResultLine`, a `Callout`), and a toast lands in the region's polite live
 * viewport. The Remove confirm names what it costs on a red button.
 */
const RESULTS: readonly (readonly [
	window: "main" | "settings" | "onboarding",
	query: string,
	testId: string,
])[] = [
	["settings", "section=export&scenario=export-on", "export-status"],
	["settings", "section=general&scenario=settings-error", "section-error"],
	["onboarding", "scenario=onboarding-setup", "onboarding-vault-validation"],
	["main", "scenario=export-failed&tab=summary", "export-status"],
	[
		"settings",
		"section=summaries&scenario=summaries-failed",
		"summaries-status",
	],
];

for (const [window, query, testId] of RESULTS) {
	test(`${window} live region ${testId}`, async ({ page }) => {
		await page.goto(`/#/${window}?${query}`);
		await settle(page);
		const line = page.getByTestId(testId).first();
		await expect(line).toBeVisible();
		const announced = await line.evaluate(
			(element) =>
				element.closest("[role=status],[role=alert],[aria-live]") !== null ||
				element.querySelector("[role=status],[role=alert],[aria-live]") !==
					null,
		);
		expect(announced).toBe(true);
	});
}

for (const scheme of SCHEMES) {
	const dark = scheme === "dark" ? "&dark" : "";

	test(`main ${scheme} toast`, async ({ page, context }) => {
		await context.grantPermissions(["clipboard-read", "clipboard-write"]);
		await page.setViewportSize({ width: 1200, height: 760 });
		await page.goto(`/#/main?tab=transcript${dark}`);
		await settle(page);
		await page.getByTestId("meeting-actions").click();
		await page.getByTestId("copy-transcript").click();
		const toast = page.getByTestId("toast");
		await expect(toast).toContainText("Transcript copied.");
		expect(
			await toast.evaluate(
				(element) => element.closest("[aria-live]") !== null,
			),
		).toBe(true);
		await page.waitForTimeout(300);
		await page.screenshot({ path: `${OUT}/main-${scheme}-toast.png` });
	});

	test(`settings ${scheme} confirm remove`, async ({ page }) => {
		await page.setViewportSize(SETTINGS_SIZE);
		await page.goto(`/#/settings?section=iphone${dark}`);
		await settle(page);
		await page.locator("[data-testid^=revoke-]").first().click();
		const dialog = page.getByTestId("confirm-dialog");
		await expect(dialog).toBeVisible();
		await expect(dialog).toContainText("can no longer send recordings");
		await expect(page.getByTestId("confirm-cancel")).toBeFocused();
		await settle(page);
		await page.screenshot({
			path: `${OUT}/settings-${scheme}-confirm-remove.png`,
		});
	});
}
