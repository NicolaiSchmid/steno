import { defineConfig, devices } from "@playwright/test";

// Screens are review evidence (plan Decision 8). `pnpm screens` builds first
// and renders the bundle through `vite preview`, the same files the app
// serves offline.
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
		command: "pnpm exec vite preview --port 4173 --strictPort",
		url: "http://localhost:4173",
		reuseExistingServer: false,
		timeout: 30_000,
	},
});
