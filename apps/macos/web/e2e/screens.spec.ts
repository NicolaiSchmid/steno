import { mkdirSync } from "node:fs";
import { expect, type Page, test } from "@playwright/test";

/**
 * Renders the main window in every state and the stories, in light and dark
 * at both reference window sizes, and writes PNGs to screens/. Every page
 * must make zero requests to anything but the preview server (plan
 * Decision 9). The main window runs over the fixture bridge; `scenario=`
 * and `tab=` bend the fixtures (`src/bridge/mock-transport.ts`).
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
