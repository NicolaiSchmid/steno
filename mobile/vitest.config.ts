import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

export default defineConfig({
	// The `@/` and `@modules/` path aliases from tsconfig, so tests can
	// vi.mock("@/lib/…") the same way the code imports it.
	resolve: {
		alias: {
			"@modules": fileURLToPath(new URL("./modules", import.meta.url)),
			"@": fileURLToPath(new URL("./src", import.meta.url)),
		},
	},
	test: {
		environment: "node",
		include: ["src/**/*.test.ts", "src/**/*.test.tsx", "modules/**/*.test.ts"],
		server: {
			deps: {
				// Native modules are mocked by mounted-hook tests; do not parse
				// their Flow source.
				external: [/react-native/, /expo-/],
			},
		},
	},
});
