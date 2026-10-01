import { mkdirSync } from "node:fs";
import { expect, type Page, test } from "@playwright/test";

/**
 * Renders the main window in every state at both reference window sizes,
 * the Settings window's six sections and notable states at its fixed 760 by
 * 520, and the stories, each in light and dark, and writes PNGs to
 * screens/. Every page must make zero requests to anything but the preview
 * server (plan Decision 9). The windows run over the fixture bridge;
 * `scenario=` and `tab=` bend the fixtures (`src/bridge/mock-transport.ts`).
 */

const OUT = "screens";
const SIZES = [
	{ width: 960, height: 600 },
	{ width: 1200, height: 760 },
] as const;
const SCHEMES = ["light", "dark"] as const;

interface MainState {
	name: string;
	query: string;
	/** What must be on screen before the shot. */
	expect: { testId?: string; role?: "menu" | "dialog" | "option" };
	/** Scrolled into view before the shot (the footer sits below the fold). */
	scrollTo?: string;
}

const MAIN_STATES: readonly MainState[] = [
	{ name: "summary", query: "tab=summary", expect: { testId: "tab-summary" } },
	{
		name: "transcript-picker",
		query: "tab=transcript&picker",
		expect: { role: "option" },
	},
	{
		name: "tasks",
		query: "tab=tasks",
		expect: { testId: "tab-content-tasks" },
	},
	{
		name: "notes",
		query: "tab=notes",
		expect: { testId: "scratchpad-editor" },
	},
	{ name: "menu", query: "tab=summary&menu", expect: { role: "menu" } },
	{
		name: "empty",
		query: "scenario=empty",
		expect: { testId: "empty-meetings-title" },
	},
	{
		name: "recording",
		query: "scenario=recording",
		expect: { testId: "auto-stop" },
	},
	{
		name: "denied",
		query: "scenario=denied",
		expect: { testId: "denied-microphone" },
	},
	{
		name: "failed",
		query: "scenario=failed",
		expect: { testId: "summary-try-again" },
	},
	{
		name: "processing",
		query: "scenario=processing",
		expect: { testId: "processing-card" },
	},
	{
		name: "export-failed",
		query: "scenario=export-failed&tab=summary",
		expect: { testId: "export-again" },
		scrollTo: "meeting-footer",
	},
];

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

for (const size of SIZES) {
	for (const scheme of SCHEMES) {
		for (const state of MAIN_STATES) {
			test(`main ${scheme} ${size.width}x${size.height} ${state.name}`, async ({
				page,
			}) => {
				const external = watchExternalRequests(page);
				await page.setViewportSize(size);
				const dark = scheme === "dark" ? "&dark" : "";
				await page.goto(`/#/main?${state.query}${dark}`);
				await settle(page);
				await expect(page.getByTestId("main-window")).toBeVisible();
				if (state.expect.testId) {
					await expect(page.getByTestId(state.expect.testId)).toBeVisible();
				}
				if (state.expect.role) {
					await expect(page.getByRole(state.expect.role).first()).toBeVisible();
				}
				if (state.name === "empty") {
					await expect(page.getByTestId("empty-detail-title")).toBeVisible();
				}
				if (state.scrollTo) {
					await page.getByTestId(state.scrollTo).scrollIntoViewIfNeeded();
				}
				await settle(page);
				await page.screenshot({
					path: `${OUT}/main-${scheme}-${size.width}x${size.height}-${state.name}.png`,
				});
				expect(external).toEqual([]);
			});
		}
	}
}

/** The Settings window is one fixed size (plan Decision 1). */
const SETTINGS_SIZE = { width: 760, height: 520 } as const;

interface SettingsState {
	name: string;
	/** `section=` plus any scenario the state needs. */
	query: string;
	/** What must be on screen before the shot. */
	testId: string;
	/** Clicked before the shot (a dialog trigger). */
	click?: string;
}

const SETTINGS_STATES: readonly SettingsState[] = [
	{ name: "general", query: "section=general", testId: "update-status" },
	{
		name: "general-error",
		query: "section=general&scenario=settings-error",
		testId: "login-item-approval",
	},
	{
		name: "acknowledgements",
		query: "section=general",
		testId: "acknowledgements-dialog",
		click: "acknowledgements",
	},
	{ name: "recording", query: "section=recording", testId: "retention-mode" },
	{
		name: "transcription",
		query: "section=transcription",
		testId: "asset-offlineDiarizer-progress",
	},
	{
		name: "transcription-failed",
		query: "section=transcription&scenario=download-failed",
		testId: "asset-parakeetV3-retry",
	},
	{ name: "summaries", query: "section=summaries", testId: "summaries-status" },
	{
		name: "summaries-connected",
		query: "section=summaries&scenario=summaries-connected",
		testId: "test-connection",
	},
	{
		name: "summaries-failed",
		query: "section=summaries&scenario=summaries-failed",
		testId: "test-result-details",
	},
	{
		name: "codex-consent",
		query: "section=summaries&scenario=codex-consent",
		testId: "codex-confirm",
	},
	{
		name: "codex",
		query: "section=summaries&scenario=codex",
		testId: "codex-model",
	},
	{ name: "export", query: "section=export", testId: "export-enabled" },
	{
		name: "export-on",
		query: "section=export&scenario=export-on",
		testId: "export-status",
	},
	{ name: "iphone", query: "section=iphone", testId: "begin-pairing" },
	{
		name: "pairing",
		query: "section=iphone&scenario=pairing",
		testId: "pairing-card",
	},
	{
		name: "iphone-unavailable",
		query: "section=iphone&scenario=phone-unavailable",
		testId: "phone-unavailable",
	},
];

for (const scheme of SCHEMES) {
	for (const state of SETTINGS_STATES) {
		test(`settings ${scheme} ${state.name}`, async ({ page }) => {
			const external = watchExternalRequests(page);
			await page.setViewportSize(SETTINGS_SIZE);
			const dark = scheme === "dark" ? "&dark" : "";
			await page.goto(`/#/settings?${state.query}${dark}`);
			await settle(page);
			await expect(page.getByTestId("settings-window")).toBeVisible();
			if (state.click) {
				await page.getByTestId(state.click).click();
			}
			await expect(page.getByTestId(state.testId)).toBeVisible();
			await settle(page);
			await page.screenshot({
				path: `${OUT}/settings-${scheme}-${state.name}.png`,
			});
			expect(external).toEqual([]);
		});
	}
}

for (const scheme of SCHEMES) {
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
		await page.setViewportSize({
			width: 1200,
			height: Math.min(height, 8000),
		});
		await settle(page);
		await page.screenshot({ path: `${OUT}/stories-${scheme}.png` });
		expect(external).toEqual([]);
	});
}
