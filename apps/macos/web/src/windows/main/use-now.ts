import { useEffect, useState } from "react";

/**
 * The current time, re-read every `intervalMs` while `active`. Drives the
 * elapsed counters at 1 Hz; nothing else in the page ticks.
 */
export function useNow(active: boolean, intervalMs = 1000): number {
	const [now, setNow] = useState(() => Date.now());
	useEffect(() => {
		if (!active) {
			return;
		}
		setNow(Date.now());
		const timer = setInterval(() => setNow(Date.now()), intervalMs);
		return () => clearInterval(timer);
	}, [active, intervalMs]);
	return now;
}

/** Whole seconds since `startedAt`, ticking; zero when not started. */
export function useElapsedSeconds(startedAt: string | undefined): number {
	const now = useNow(startedAt !== undefined);
	if (!startedAt) {
		return 0;
	}
	return Math.max(0, Math.floor((now - new Date(startedAt).getTime()) / 1000));
}
