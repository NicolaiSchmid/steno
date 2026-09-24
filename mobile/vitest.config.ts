import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

export default defineConfig({
	// The `@/` path alias from tsconfig, so tests can vi.mock("@/lib/…") the
	// same way the code imports it.
	resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
	test: {
		environment: "node",
		include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
		server: {
			deps: {
				// Native modules are mocked by mounted-hook tests; do not parse
				// their Flow source.
				external: [/react-native/, /expo-/],
			},
		},
	},
});
