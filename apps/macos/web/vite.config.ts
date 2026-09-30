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

// The bundle is served from the app scheme (steno-app://app/), so every asset
// URL must be relative. Nothing here may reach the network at runtime;
// scripts/check-offline.mjs greps dist/ after every build.
export default defineConfig({
	base: "./",
	plugins: [react(), tailwindcss(), stripCssBanners()],
	resolve: {
		alias: {
			"@": fileURLToPath(new URL("./src", import.meta.url)),
		},
	},
	build: {
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
});
