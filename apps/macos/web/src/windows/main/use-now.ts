import { useEffect, useMemo, useState } from "react";

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

/**
 * Seconds left, counted down at 1 Hz from `seconds` as of the render it
 * arrived in; a new figure re-anchors the count. Goes below zero once the
 * time is up; `format.countdown` shows that as 0:00.
 */
export function useCountdownSeconds(seconds: number): number {
	const anchor = useMemo(() => ({ at: Date.now(), seconds }), [seconds]);
	const now = useNow(true);
	return anchor.seconds - (now - anchor.at) / 1000;
}

/** Whole seconds since `startedAt`, ticking; zero when not started. */
export function useElapsedSeconds(startedAt: string | undefined): number {
	const now = useNow(startedAt !== undefined);
	if (!startedAt) {
		return 0;
	}
	return Math.max(0, Math.floor((now - new Date(startedAt).getTime()) / 1000));
}
