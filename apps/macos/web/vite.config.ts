import { fileURLToPath } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig, type Plugin } from "vitest/config";

/** Drops `/*! ... *\/` license banners (they carry URLs) from emitted CSS. */
function stripCssBanners(): Plugin {
	return {
		name: "steno:strip-css-banners",
		generateBundle(_options, bundle) {
			for (const asset of Object.values(bundle)) {
				if (asset.type === "asset" && asset.fileName.endsWith(".css")) {
					const source =
						typeof asset.source === "string"
							? asset.source
							: new TextDecoder().decode(asset.source);
					asset.source = source.replace(/\/\*![\s\S]*?\*\//g, "");
				}
			}
		},
	};
}

const srcDir = fileURLToPath(new URL("./src", import.meta.url));

// The bundle is served from the app scheme (steno-app://app/), so every asset
// URL must be relative. Nothing here may reach the network at runtime;
// scripts/check-offline.mjs greps dist/ after every build.
//
// Two bundles: `vite build` (mode production) is what scripts/build-web.sh
// copies into the app, without the mock transport or the recorded fixtures
// (`#bridge-fallback` resolves to a thrown error; scripts/check-bundle.mjs
// fails the build if either lands in dist/). `vite build --mode screens`
// writes dist-screens/ with the fixture bridge for Playwright and
// `vite preview --mode screens`; the dev server and Vitest keep it too.
export default defineConfig(({ command, mode }) => {
	const screens = mode === "screens";
	// Opt-in, not the default for an unknown mode: the dev server, Vitest
	// and the screens build get the fixture-backed mock; anything else gets
	// the fallback that throws.
	const mockBridge =
		command === "serve" || mode === "screens" || mode === "test";
	return {
		base: "./",
		plugins: [react(), tailwindcss(), stripCssBanners()],
		resolve: {
			alias: {
				"@": srcDir,
				"#bridge-fallback": `${srcDir}/bridge/${
					mockBridge ? "fallback-mock.ts" : "fallback-none.ts"
				}`,
			},
		},
		build: {
			outDir: screens ? "dist-screens" : "dist",
			target: "safari18",
			sourcemap: false,
			modulePreload: { polyfill: false },
			// One local bundle; splitting it buys nothing over the app scheme.
			chunkSizeWarningLimit: 700,
		},
		server: {
			port: 5173,
			strictPort: true,
		},
		preview: {
			port: 4173,
			strictPort: true,
		},
		test: {
			environment: "jsdom",
			globals: false,
			setupFiles: ["./src/test/setup.ts"],
			include: ["src/**/*.test.{ts,tsx}", "scripts/**/*.test.ts"],
			css: false,
		},
	};
});
