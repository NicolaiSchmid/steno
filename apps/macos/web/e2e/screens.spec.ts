import { mkdirSync } from "node:fs";
import { expect, type Page, test } from "@playwright/test";

/**
 * Renders the main window in every state at both reference window sizes,
 * the Settings window's sections and states at its 960 by 640 default size,
 * the onboarding window at its fixed 560 by 620 and the stories, each in
 * light and dark, to screens/. Every page must make zero requests to anything
 * but the preview server. `scenario=` and `tab=` bend the fixtures
 * (`src/bridge/mock-transport.ts`).
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
	["failed", "scenario=failed", "summary-try-again"],
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
const SCROLLS: Record<string, string> = { "export-failed": "meeting-footer" };

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
