import { defineConfig, devices } from "@playwright/test";

// Screens are review evidence (plan Decision 8). `pnpm screens` builds the
// screens bundle first (`vite build --mode screens`: the app's bundle plus
// the fixture bridge, in dist-screens/) and renders it through
// `vite preview --mode screens`.
export default defineConfig({
	testDir: "./e2e",
	outputDir: "./test-results",
	fullyParallel: true,
	retries: 0,
	reporter: [["list"]],
	use: {
		...devices["Desktop Chrome"],
		baseURL: "http://localhost:4173",
		deviceScaleFactor: 2,
		colorScheme: "light",
	},
	projects: [{ name: "chromium", use: { browserName: "chromium" } }],
	webServer: {
		command: "pnpm exec vite preview --mode screens --port 4173 --strictPort",
		url: "http://localhost:4173",
		reuseExistingServer: false,
		timeout: 30_000,
	},
});
