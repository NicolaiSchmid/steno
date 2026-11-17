/** How many entries the hosts' people palette has (`PEOPLE_PALETTE_SIZE`). */
const PEOPLE_PALETTE_SIZE = 8;

/**
 * A person's palette index as both hosts fold it (`color_index` in
 * `steno-host`, `MainWindowSnapshots.colorIndex` in Swift): FNV-1a over the
 * ID's 16 bytes. A voice takes its person's colour, so an option's avatar
 * shows the colour the voice will have once the pick is confirmed.
 */
export function personColor(personID: string): number {
	const hex = personID.replace(/-/g, "");
	let hash = 2_166_136_261;
	for (let at = 0; at + 2 <= hex.length; at += 2) {
		hash ^= Number.parseInt(hex.slice(at, at + 2), 16);
		hash = Math.imul(hash, 16_777_619) >>> 0;
	}
	return hash % PEOPLE_PALETTE_SIZE;
}
