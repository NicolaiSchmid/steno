import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import config from "../app.config";

// Rendered by apps/macos/scripts/make-app-icon.sh; the Swift AppIconTests
// only run when apps/macos/** changes, so the mobile lane checks the file
// it ships on its own.
const ICON = "./assets/icon.png";
const PNG_SIGNATURE = Buffer.from([
	0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a,
]);

describe("app.config icon", () => {
	it("points icon and ios.icon at the rendered PNG", () => {
		expect(config.icon).toBe(ICON);
		expect(config.ios?.icon).toEqual({ light: ICON, dark: ICON });
	});

	it("ships an opaque 1024 px PNG at that path", () => {
		const path = fileURLToPath(new URL(`../${ICON}`, import.meta.url));
		expect(existsSync(path)).toBe(true);
		// Signature, then the IHDR chunk: width at 16, height at 20, colour type
		// at 25. Colour type 2 is truecolour without alpha, which App Store
		// Connect requires for the 1024 px icon.
		const header = readFileSync(path).subarray(0, 29);
		expect(header.subarray(0, 8)).toEqual(PNG_SIGNATURE);
		expect(header.readUInt32BE(16)).toBe(1024);
		expect(header.readUInt32BE(20)).toBe(1024);
		expect(header[25]).toBe(2);
	});
});
