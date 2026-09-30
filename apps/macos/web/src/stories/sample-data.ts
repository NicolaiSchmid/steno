/** Hard-coded sample content from docs/design/webview-mockup/steno-main.html. */

export interface SamplePerson {
	name: string;
	index: number;
	unknown?: boolean;
}

export interface SampleMeeting {
	id: string;
	title: string;
	time: string;
	preview: string;
	kind: "Call" | "In person";
	duration: string;
	people: SamplePerson[];
	noSummary?: boolean;
}

export interface SampleDay {
	label: string;
	date: string;
	meetings: SampleMeeting[];
}

export const people = {
	nicolai: { name: "Nicolai", index: 0 },
	jerome: { name: "Jérôme", index: 1 },
	anna: { name: "Anna", index: 2 },
	unknown: { name: "Unknown speaker", index: 3, unknown: true },
} satisfies Record<string, SamplePerson>;

export const days: SampleDay[] = [
	{
		label: "Today",
		date: "Tuesday, Sep 29",
		meetings: [
			{
				id: "m1",
				title: "Produktstrategie 90/10",
				time: "14:50",
				preview:
					"Nicolai schlägt vor, 90 Prozent auf den Kern zu setzen. Jérôme prüft die Zahlen bis Freitag, Anna informiert die Partner.",
				kind: "Call",
				duration: "45:38",
				people: [people.nicolai, people.jerome, people.anna, people.unknown],
			},
			{
				id: "m2",
				title: "Standup",
				time: "14:06",
				preview:
					"Kurzer Abgleich zum Release. rc.3 ist draußen, das Settings-Fenster bleibt offen.",
				kind: "Call",
				duration: "12:04",
				people: [people.nicolai, people.jerome],
			},
			{
				id: "m3",
				title: "Tuesday 10:08",
				time: "10:08",
				preview:
					"Transcript ready. No summary yet: summaries are off until an AI service is set up.",
				kind: "In person",
				duration: "38:11",
				people: [],
				noSummary: true,
			},
		],
	},
	{
		label: "Yesterday",
		date: "Monday, Sep 28",
		meetings: [
			{
				id: "m4",
				title: "Investor update prep",
				time: "15:46",
				preview:
					"Deck bis Donnerstag, Zahlen aus dem Q3-Report, Anna übernimmt die Grafiken.",
				kind: "Call",
				duration: "27:30",
				people: [people.nicolai, people.anna],
			},
			{
				id: "m5",
				title: "Monday 11:20",
				time: "11:20",
				preview: "Transcript only. Recording kept.",
				kind: "In person",
				duration: "52:02",
				people: [],
			},
		],
	},
];

export const filters = [
	{ id: "all", label: "All", count: 7 },
	{ id: "progress", label: "In progress", count: 0 },
	{ id: "ready", label: "Ready", count: 7 },
	{ id: "failed", label: "Failed", count: 0 },
] as const;

export const tags = ["strategie", "q4", "investors"];

export interface SummaryItem {
	lead: string;
	text: string;
	timestamp?: string;
}

export interface SummarySection {
	heading: string;
	items: SummaryItem[];
}

export const summarySections: SummarySection[] = [
	{
		heading: "Executive summary",
		items: [
			{
				lead: "Fokus.",
				text: "Nicolai schlägt vor, 90 Prozent der Kapazität auf den Kern zu setzen und Nebenprojekte bis Q1 zu pausieren.",
			},
			{
				lead: "Budget.",
				text: "Jérôme prüft die Zahlen bis Freitag und bringt zwei Szenarien mit.",
			},
			{
				lead: "Team.",
				text: "Anna übernimmt die Kommunikation an die Partner, sobald die Entscheidung steht.",
			},
		],
	},
	{
		heading: "Open questions",
		items: [
			{
				lead: "Zeitplan.",
				text: "Start im Oktober oder erst im November nach dem Investor-Update?",
				timestamp: "01:30",
			},
		],
	},
];

export const detail = {
	kind: "Call",
	when: "Tuesday, Sep 29 · 14:50",
	duration: "45:38",
	language: "German",
	retention: "Deletes Oct 29",
	title: "Produktstrategie 90/10",
	people: [people.nicolai, people.jerome, people.anna, people.unknown],
	names: "Nicolai, Jérôme, Anna +1",
	tags: ["strategie", "q4"],
	turnCount: 142,
	taskCount: 3,
	summary: summarySections,
	tasks: [
		{
			id: "t1",
			text: "Zahlen für beide Szenarien",
			meta: "Jérôme · Friday",
			done: false,
		},
		{
			id: "t2",
			text: "Partner-Mail vorbereiten",
			meta: "Anna · after the investor update",
			done: false,
		},
		{
			id: "t3",
			text: "Entscheidung im Investor-Update ansprechen",
			meta: "Nicolai",
			done: true,
		},
	],
	turns: [
		{
			id: "u1",
			who: "Nicolai",
			range: "00:12 – 00:41",
			text: "Lass uns kurz auf die Prioritäten schauen. Ich würde vorschlagen, dass wir neunzig Prozent auf den Kern setzen und den Rest erst mal ruhen lassen. Wir haben in den letzten zwei Quartalen zu viel parallel gemacht, und am Ende ist keins der Nebenprojekte wirklich fertig geworden.",
		},
		{
			id: "u2",
			who: "Jérôme",
			range: "00:41 – 01:05",
			text: "Das geht nur, wenn wir das Budget entsprechend umschichten. Ich kann bis Freitag zwei Szenarien rechnen, eins konservativ, eins mit der offenen Stelle. Wichtig wäre mir, dass wir die Zahlen dann auch wirklich als Grundlage nehmen.",
			highlight: "Budget",
		},
		{
			id: "u3",
			who: "Speaker 3",
			range: "01:05 – 01:30",
			text: "Die Partner sollten das nicht aus zweiter Hand hören. Wenn wir das entscheiden, schreibe ich ihnen, aber erst nach dem Investor-Update, sonst haben wir zwei Gespräche gleichzeitig laufen.",
			unnamed: true,
		},
		{
			id: "u4",
			who: "Nicolai",
			range: "01:30 – 01:52",
			text: "Einverstanden. Dann halten wir fest: neunzig Prozent auf den Kern, Nebenprojekte pausieren bis Q1, Kommunikation nach dem Update. Jérôme, du bringst die beiden Budget-Szenarien mit.",
			highlight: "Budget",
		},
	],
};

export interface TextSegment {
	/** The character offset in the source text; stable across renders. */
	offset: number;
	text: string;
	marked: boolean;
}

/** Splits `text` into plain and marked runs around every `term`. */
export function highlightSegments(text: string, term?: string): TextSegment[] {
	if (!term) {
		return [{ offset: 0, text, marked: false }];
	}
	const segments: TextSegment[] = [];
	let cursor = 0;
	for (;;) {
		const at = text.indexOf(term, cursor);
		if (at < 0) {
			break;
		}
		if (at > cursor) {
			segments.push({
				offset: cursor,
				text: text.slice(cursor, at),
				marked: false,
			});
		}
		segments.push({ offset: at, text: term, marked: true });
		cursor = at + term.length;
	}
	if (cursor < text.length) {
		segments.push({ offset: cursor, text: text.slice(cursor), marked: false });
	}
	return segments;
}

export const scrollLines = [
	"Lass uns kurz auf die Prioritäten schauen.",
	"Neunzig Prozent auf den Kern, der Rest ruht.",
	"Zu viel parallel in den letzten zwei Quartalen.",
	"Das geht nur mit umgeschichtetem Budget.",
	"Zwei Szenarien bis Freitag.",
	"Die Zahlen als Grundlage nehmen.",
	"Die Partner nicht aus zweiter Hand informieren.",
	"Erst nach dem Investor-Update schreiben.",
	"Nebenprojekte pausieren bis Q1.",
	"Kommunikation nach dem Update.",
	"Jérôme bringt die Budget-Szenarien mit.",
	"Anna übernimmt die Partner-Mail.",
	"Entscheidung im Investor-Update ansprechen.",
	"Nächster Abgleich am Freitag.",
];
