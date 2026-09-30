import { mkdirSync } from "node:fs";
import { expect, type Page, test } from "@playwright/test";

/**
 * Renders the stories and the shell preview in light and dark at both
 * reference window sizes and writes PNGs to screens/. Every page must make
 * zero requests to anything but the preview server (plan Decision 9).
 */

const OUT = "screens";
const SIZES = [
	{ width: 960, height: 600 },
	{ width: 1200, height: 760 },
] as const;
const SCHEMES = ["light", "dark"] as const;
const SHELL_STATES = [
	{ name: "summary", query: "tab=summary" },
	{ name: "transcript", query: "tab=transcript" },
	{ name: "transcript-menu", query: "tab=transcript&menu" },
] as const;

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
		for (const state of SHELL_STATES) {
			test(`shell ${scheme} ${size.width}x${size.height} ${state.name}`, async ({
				page,
			}) => {
				const external = watchExternalRequests(page);
				await page.setViewportSize(size);
				const dark = scheme === "dark" ? "&dark" : "";
				await page.goto(`/#/shell?${state.query}${dark}`);
				await settle(page);
				await expect(page.getByTestId("shell")).toBeVisible();
				if (state.name === "transcript-menu") {
					await expect(page.getByRole("menu")).toBeVisible();
				}
				await page.screenshot({
					path: `${OUT}/shell-${scheme}-${size.width}x${size.height}-${state.name}.png`,
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
