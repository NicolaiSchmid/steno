import type { MeetingDetailSnapshot, MeetingRow } from "@/bridge/contract";

/**
 * Every number, time and date the main window shows, through `Intl` so the
 * words follow the user's language and calendar. `createFormatter` takes a
 * locale and time zone so tests pin both; the page uses the browser's.
 */

export interface FormatOptions {
	locale?: string;
	timeZone?: string;
}

export interface DayLabel {
	/** "Today", "Yesterday" or the weekday. */
	label: string;
	/** The date beside it: the full date for today and yesterday, else short. */
	date: string;
}

export interface Formatter {
	/** `45:38`, or `1:02:03` past an hour. */
	duration(seconds: number): string;
	/** `0:42`, `1:05`: seconds left, the minutes without a leading zero. */
	countdown(seconds: number): string;
	/** `00:12 – 00:41`. */
	range(startSeconds: number, endSeconds: number): string;
	/** `14:50`. */
	time(iso: string): string;
	/** `Tuesday, Sep 29`. */
	date(iso: string): string;
	/** `Sep 29`, with the year once it is not the current one. */
	shortDate(iso: string, now?: Date): string;
	/** `Tuesday, Sep 29 · 14:50`. */
	when(iso: string): string;
	/** The heading of a day group from its `YYYY-MM-DD` key. */
	dayLabel(day: string, now?: Date): DayLabel;
	/** `2 min. ago`, `yesterday`. */
	relative(iso: string, now?: Date): string;
	/** `German` for `de`; the code itself when unknown. */
	language(code: string): string;
	/** `about 2 min left`, `less than a minute left`. */
	remaining(seconds: number): string;
	/** The weekday within the coming week, else the short date. */
	dueDate(iso: string, now?: Date): string;
}

const DAY_MS = 24 * 60 * 60 * 1000;

function pad(n: number): string {
	return n < 10 ? `0${n}` : String(n);
}

export function createFormatter(options: FormatOptions = {}): Formatter {
	const locale = options.locale;
	const zone = options.timeZone ? { timeZone: options.timeZone } : {};
	/** A date formatter in the configured zone, or in `timeZone` when given. */
	function formatter(parts: Intl.DateTimeFormatOptions, timeZone?: string) {
		return new Intl.DateTimeFormat(
			locale,
			timeZone ? { ...parts, timeZone } : { ...parts, ...zone },
		);
	}
	const DATE = { weekday: "long", month: "short", day: "numeric" } as const;
	const SHORT = { month: "short", day: "numeric" } as const;
	const SHORT_YEAR = { ...SHORT, year: "numeric" } as const;
	const WEEKDAY = { weekday: "long" } as const;

	const timeFormat = formatter({
		hour: "2-digit",
		minute: "2-digit",
		hourCycle: "h23",
	});
	const dateFormat = formatter(DATE);
	const shortDateFormat = formatter(SHORT);
	const shortDateYearFormat = formatter(SHORT_YEAR);
	const weekdayFormat = formatter(WEEKDAY);
	// `en-CA` writes YYYY-MM-DD; the calendar key the host uses for groups.
	const dayKeyFormat = new Intl.DateTimeFormat("en-CA", {
		year: "numeric",
		month: "2-digit",
		day: "2-digit",
		...zone,
	});
	const relativeFormat = new Intl.RelativeTimeFormat(locale, {
		numeric: "auto",
		style: "short",
	});
	const languageNames = new Intl.DisplayNames(locale ? [locale] : [], {
		type: "language",
		fallback: "none",
	});

	// A `YYYY-MM-DD` key as a Date at noon UTC, so weekday formatting in UTC
	// gives that calendar day whatever the zone.
	const dayFormats = {
		weekday: formatter(WEEKDAY, "UTC"),
		date: formatter(DATE, "UTC"),
		short: formatter(SHORT, "UTC"),
		shortYear: formatter(SHORT_YEAR, "UTC"),
	};

	function dayKeyToDate(day: string): Date {
		const [y = 0, m = 1, d = 1] = day.split("-").map(Number);
		return new Date(Date.UTC(y, m - 1, d, 12));
	}

	function yearOf(date: Date): number {
		const parts = dayKeyFormat.formatToParts(date);
		return Number(parts.find((part) => part.type === "year")?.value ?? 0);
	}

	return {
		duration(seconds) {
			const total = Math.max(0, Math.round(seconds));
			const hours = Math.floor(total / 3600);
			const minutes = Math.floor((total % 3600) / 60);
			const rest = total % 60;
			return hours > 0
				? `${hours}:${pad(minutes)}:${pad(rest)}`
				: `${pad(minutes)}:${pad(rest)}`;
		},
		countdown(seconds) {
			const total = Math.max(0, Math.ceil(seconds));
			return `${Math.floor(total / 60)}:${pad(total % 60)}`;
		},
		range(start, end) {
			return `${this.duration(start)} – ${this.duration(end)}`;
		},
		time(iso) {
			return timeFormat.format(new Date(iso));
		},
		date(iso) {
			return dateFormat.format(new Date(iso));
		},
		shortDate(iso, now = new Date()) {
			const date = new Date(iso);
			return yearOf(date) === yearOf(now)
				? shortDateFormat.format(date)
				: shortDateYearFormat.format(date);
		},
		when(iso) {
			return `${this.date(iso)} · ${this.time(iso)}`;
		},
		dayLabel(day, now = new Date()) {
			const today = dayKeyFormat.format(now);
			const yesterday = dayKeyFormat.format(new Date(now.getTime() - DAY_MS));
			const date = dayKeyToDate(day);
			if (day === today) {
				return { label: "Today", date: dayFormats.date.format(date) };
			}
			if (day === yesterday) {
				return { label: "Yesterday", date: dayFormats.date.format(date) };
			}
			const sameYear = day.slice(0, 4) === today.slice(0, 4);
			return {
				label: dayFormats.weekday.format(date),
				date: sameYear
					? dayFormats.short.format(date)
					: dayFormats.shortYear.format(date),
			};
		},
		relative(iso, now = new Date()) {
			const seconds = Math.round(
				(new Date(iso).getTime() - now.getTime()) / 1000,
			);
			const abs = Math.abs(seconds);
			if (abs < 60) {
				return relativeFormat.format(0, "second").replace(/^in /, "");
			}
			if (abs < 3600) {
				return relativeFormat.format(Math.trunc(seconds / 60), "minute");
			}
			if (abs < DAY_MS / 1000) {
				return relativeFormat.format(Math.trunc(seconds / 3600), "hour");
			}
			return relativeFormat.format(
				Math.trunc(seconds / (DAY_MS / 1000)),
				"day",
			);
		},
		language(code) {
			try {
				return languageNames.of(code) ?? code;
			} catch {
				return code;
			}
		},
		remaining(seconds) {
			if (seconds < 60) {
				return "less than a minute left";
			}
			const minutes = Math.ceil(seconds / 60);
			return minutes === 1
				? "about a minute left"
				: `about ${minutes} min left`;
		},
		dueDate(iso, now = new Date()) {
			const date = new Date(iso);
			const days = (date.getTime() - now.getTime()) / DAY_MS;
			if (days >= 0 && days < 6) {
				const key = dayKeyFormat.format(date);
				if (key === dayKeyFormat.format(now)) {
					return "Today";
				}
				if (key === dayKeyFormat.format(new Date(now.getTime() + DAY_MS))) {
					return "Tomorrow";
				}
				return weekdayFormat.format(date);
			}
			return this.shortDate(iso, now);
		},
	};
}

/** The page's formatter: the browser's language and time zone. */
export const format: Formatter = createFormatter();

/** The meeting kind as a word: what the row badge and the eyebrow show. */
export function formatSource(source: MeetingRow["source"]): string {
	switch (source) {
		case "call":
			return "Call";
		case "inPerson":
			return "In person";
		case "phone":
			return "iPhone";
	}
}

/** One clause for the eyebrow about the recording's fate. */
export function formatRetention(
	retention: MeetingDetailSnapshot["retention"],
	formatter: Formatter = format,
	now: Date = new Date(),
): string {
	switch (retention.kind) {
		case "deleted":
			return "Recording deleted";
		case "deletesOn":
			return retention.deletesAt
				? `Deletes ${formatter.shortDate(retention.deletesAt, now)}`
				: "Recording deleted after processing";
		case "keptUntilExportSucceeds":
			return "Recording kept until the export succeeds";
		case "keptProcessingFailed":
			return "Recording kept because processing failed";
		case "keptIncomplete":
			return "Recording kept because the speakers or the transcript may be incomplete. Process again, or delete the recording";
		case "keptWhileProcessing":
			return "Recording kept while processing";
		case "keptForever":
			return "Recording kept";
	}
}

/**
 * The first sentence of a failure reason: up to the first full stop that
 * ends a sentence on the first non-blank line, or that line whole.
 */
export function firstSentence(reason: string): string {
	const firstLine =
		reason
			.split(/\r?\n/)
			.map((line) => line.trim())
			.find((line) => line.length > 0) ?? "";
	if (!firstLine) {
		return "No reason was reported.";
	}
	const match = /^.*?\.(?=\s|$)/.exec(firstLine);
	return match ? match[0] : firstLine;
}

/** "Nicolai, Jérôme, Anna +1": the named people, then how many are unnamed. */
export function formatPeople(
	speakers: MeetingDetailSnapshot["speakers"],
): string {
	const named = speakers
		.filter((speaker) => speaker.assignment !== "unknown")
		.map((speaker) => speaker.displayName);
	const unnamed = speakers.length - named.length;
	if (named.length === 0) {
		return unnamed === 0
			? "No speakers yet"
			: `${unnamed} unnamed ${unnamed === 1 ? "speaker" : "speakers"}`;
	}
	return unnamed > 0 ? `${named.join(", ")} +${unnamed}` : named.join(", ");
}
