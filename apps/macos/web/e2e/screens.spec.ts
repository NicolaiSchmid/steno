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
];

const openSpeakers = async (page: Page) => {
	await page.getByTestId("speakers-trigger").click();
	await expect(page.getByRole("dialog")).toBeVisible();
};

const SPEAKERS_STATES: readonly SpeakersState[] = [
	["none-closed", "scenario=speakers-none", async () => {}],
	["none", "scenario=speakers-none", openSpeakers],
	["me-closed", "scenario=speakers-me", async () => {}],
	["me", "scenario=speakers-me", openSpeakers],
	[
		"me-editing",
		"scenario=speakers-me",
		async (page) => {
			await openSpeakers(page);
			await page.getByRole("button", { name: "Name Speaker 1" }).click();
			await expect(page.getByRole("option").first()).toBeVisible();
		},
	],
	[
		"me-typing",
		"scenario=speakers-me",
		async (page) => {
			await openSpeakers(page);
			await page.getByRole("button", { name: "Name Speaker 1" }).click();
			await page.keyboard.type("Ann");
			await expect(page.getByRole("option", { name: /Create/ })).toBeVisible();
		},
	],
	["partly", "", openSpeakers],
	["split", "scenario=speakers-split", openSpeakers],
	[
		"transcript-rename",
		"scenario=speakers-me&tab=transcript",
		async (page) => {
			await page
				.getByRole("button", { name: "Speaker 1", exact: true })
				.first()
				.click();
			await expect(page.getByRole("option").first()).toBeVisible();
		},
	],
	[
		"transcript-tooltip",
		"scenario=speakers-me&tab=transcript",
		async (page) => {
			await page
				.getByRole("button", { name: "Nicolai", exact: true })
				.first()
				.hover();
			await expect(page.getByText("Click to rename")).toBeVisible();
		},
	],
];

/** The speakers button and popover, at 1200 by 760, in each naming state. */
for (const scheme of SCHEMES) {
	for (const [name, query, act] of SPEAKERS_STATES) {
		test(`main ${scheme} speakers-${name}`, async ({ page }) => {
			const external = watchExternalRequests(page);
			await page.setViewportSize({ width: 1200, height: 760 });
			const params = [query, scheme === "dark" ? "dark" : ""]
				.filter(Boolean)
				.join("&");
			await page.goto(`/#/main${params ? `?${params}` : ""}`);
			await settle(page);
			await act(page);
			await settle(page);
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
	 * 165 px here, under the 14 rem below which "Meetings /" goes.)
	 */
	test(`main ${scheme} 960 reading`, async ({ page }) => {
		await page.setViewportSize({ width: 960, height: 600 });
		const dark = scheme === "dark" ? "&dark" : "";
		await page.goto(`/#/main?tab=summary${dark}`);
		await settle(page);
		const heading = page.getByTestId("tab-content-summary").locator("h3");
		await expect(heading.first()).toBeInViewport({ ratio: 1 });
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
