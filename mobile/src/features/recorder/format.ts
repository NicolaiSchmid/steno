/** Display helpers for the recorder screen. Pure; vitest-covered. */

/** `m:ss` under an hour, `h:mm:ss` from there. Negative or NaN reads 0:00. */
export function formatDuration(seconds: number): string {
	const total = Number.isFinite(seconds) ? Math.max(0, Math.floor(seconds)) : 0;
	const h = Math.floor(total / 3600);
	const m = Math.floor((total % 3600) / 60);
	const s = total % 60;
	const mm = h > 0 ? String(m).padStart(2, "0") : String(m);
	return `${h > 0 ? `${h}:` : ""}${mm}:${String(s).padStart(2, "0")}`;
}

/** Decimal megabytes with one decimal under 100 MB, none above; kB below 1 MB. */
export function formatBytes(bytes: number): string {
	if (!Number.isFinite(bytes) || bytes < 0) return "0 kB";
	if (bytes < 1_000_000) return `${Math.round(bytes / 1000)} kB`;
	const mb = bytes / 1_000_000;
	return mb < 100 ? `${mb.toFixed(1)} MB` : `${Math.round(mb)} MB`;
}

/** Percentage of a chunked upload, from bytes uploaded so far. */
export function uploadPercent(
	byteCount: number,
	uploadedChunks: number,
	chunkSize: number,
	inFlightBytes: number,
): number {
	if (byteCount <= 0) return 0;
	const done = Math.min(byteCount, uploadedChunks * chunkSize + inFlightBytes);
	return Math.min(100, Math.max(0, Math.floor((done / byteCount) * 100)));
}

/**
 * "Today 14:05", "Yesterday 09:30", else "24 Sep 09:30". Times in the device
 * locale; the date part is deliberately short because the list is the only
 * place a recording is identified before it reaches the Mac.
 */
export function formatStartedAt(iso: string, now: Date): string {
	const date = new Date(iso);
	if (Number.isNaN(date.getTime())) return "";
	const time = date.toLocaleTimeString(undefined, {
		hour: "2-digit",
		minute: "2-digit",
	});
	const dayStart = (d: Date) =>
		new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
	const days = Math.round((dayStart(now) - dayStart(date)) / 86_400_000);
	if (days === 0) return `Today ${time}`;
	if (days === 1) return `Yesterday ${time}`;
	const day = date.toLocaleDateString(undefined, {
		day: "numeric",
		month: "short",
	});
	return `${day} ${time}`;
}
