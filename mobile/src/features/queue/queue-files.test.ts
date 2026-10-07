import { beforeEach, describe, expect, it, vi } from "vitest";

/**
 * expo-file-system's `File` over a map of paths: `text()` throws for the
 * paths in `undecodable` (the bytes read but are not text) and both reads
 * throw for those in `unreadable`. `move` throws when the target exists,
 * unless `overwrite` is set, as expo's does.
 */
const fake = vi.hoisted(() => ({
	files: new Map<string, string>(),
	undecodable: new Set<string>(),
	unreadable: new Set<string>(),
}));

vi.mock("expo-file-system", () => {
	class File {
		constructor(readonly uri: string) {}
		get exists() {
			return fake.files.has(this.uri);
		}
		async text() {
			if (fake.unreadable.has(this.uri) || fake.undecodable.has(this.uri)) {
				throw new Error("Unable to read file");
			}
			return fake.files.get(this.uri) ?? "";
		}
		async bytes() {
			if (fake.unreadable.has(this.uri)) throw new Error("Unable to read file");
			return new Uint8Array([0xff]);
		}
		move(target: File, options?: { overwrite?: boolean }) {
			const text = fake.files.get(this.uri);
			if (text === undefined) throw new Error("Source does not exist");
			if (fake.files.has(target.uri) && !options?.overwrite) {
				throw new Error("Destination already exists");
			}
			fake.files.delete(this.uri);
			fake.files.set(target.uri, text);
		}
	}
	return { File, Directory: class {}, Paths: {} };
});

import { expoQueueFiles } from "./queue-files";
import { UndecodableTextError } from "./queue-storage";

const PATH = "file:///docs/queue/index.json";

describe("expoQueueFiles.readText", () => {
	beforeEach(() => {
		fake.files.clear();
		fake.undecodable.clear();
		fake.unreadable.clear();
	});

	it("reads the text, or null when the file is missing", async () => {
		expect(await expoQueueFiles.readText(PATH)).toBeNull();
		fake.files.set(PATH, "{}");
		expect(await expoQueueFiles.readText(PATH)).toBe("{}");
	});

	it("throws UndecodableTextError when the bytes read but are not text", async () => {
		fake.files.set(PATH, "�");
		fake.undecodable.add(PATH);
		await expect(expoQueueFiles.readText(PATH)).rejects.toBeInstanceOf(
			UndecodableTextError,
		);
	});

	it("throws the read error when the file cannot be read", async () => {
		fake.files.set(PATH, "{}");
		fake.unreadable.add(PATH);
		const error = await expoQueueFiles.readText(PATH).catch((e) => e);
		expect(error).not.toBeInstanceOf(UndecodableTextError);
		expect(String(error)).toMatch(/Unable to read file/);
	});
});

describe("expoQueueFiles.move and rename", () => {
	const FROM = "file:///docs/ExpoAudio/recording-1.m4a";
	const TO = "file:///docs/queue/1.m4a";

	beforeEach(() => fake.files.clear());

	it("move never replaces a file, and rename does", async () => {
		fake.files.set(FROM, "new");
		fake.files.set(TO, "old");
		await expect(expoQueueFiles.move(FROM, TO)).rejects.toThrow(
			"Destination already exists",
		);
		expect([fake.files.get(FROM), fake.files.get(TO)]).toEqual(["new", "old"]);

		await expoQueueFiles.rename(FROM, TO);
		expect([fake.files.has(FROM), fake.files.get(TO)]).toEqual([false, "new"]);
	});

	it("move moves a file to a free name", async () => {
		fake.files.set(FROM, "new");
		await expoQueueFiles.move(FROM, TO);
		expect([fake.files.has(FROM), fake.files.get(TO)]).toEqual([false, "new"]);
	});
});
