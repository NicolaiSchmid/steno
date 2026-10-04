/**
 * The facts behind /recording-law, in one place so a review touches one file.
 * Change `reviewed` whenever a row here is re-checked against its sources.
 */

export const reviewed = "4 October 2026";

export type Consent = "one" | "all" | "mixed";

export const consentLabel: Record<Consent, string> = {
	one: "You may record",
	all: "Everyone must agree",
	mixed: "Depends",
};

export interface Jurisdiction {
	place: string;
	consent: Consent;
	note: string;
}

export const jurisdictions: Jurisdiction[] = [
	{
		place: "Germany",
		consent: "all",
		note: "§ 201 StGB makes recording non-public speech a crime, private use included. Live transcription that never stores audio is arguably outside it; no court has decided.",
	},
	{
		place: "Switzerland",
		consent: "all",
		note: "Art. 179ter StGB: a participant who records a non-public conversation without the others' consent commits an offence.",
	},
	{
		place: "France",
		consent: "all",
		note: "Art. 226-1 Code pénal covers words spoken in private. Purely professional calls are a grey area.",
	},
	{
		place: "Austria",
		consent: "one",
		note: "A participant may record. Passing the recording on without consent is an offence (§ 120 StGB).",
	},
	{
		place: "UK, Netherlands, Italy, Spain",
		consent: "one",
		note: "A participant may record for their own use. Sharing it or using it for work falls under the GDPR.",
	},
	{
		place: "US, federal law",
		consent: "one",
		note: "The Wiretap Act needs one party's consent. States can be stricter, and many are.",
	},
	{
		place:
			"California, Florida, Illinois, Maryland, Massachusetts, Montana, New Hampshire, Pennsylvania, Washington",
		consent: "all",
		note: "All parties must agree. California allows $5,000 per violation in civil claims and reaches callers outside the state; Florida and Pennsylvania treat it as a felony. A call across states follows the strictest one.",
	},
	{
		place: "Connecticut, Delaware, Michigan, Nevada, Oregon",
		consent: "mixed",
		note: "Different rules for phone calls and in-person conversations, or disputed readings. Treat them as all-party.",
	},
	{
		place: "Canada",
		consent: "one",
		note: "Criminal Code s. 184 needs one party's consent. Work use falls under PIPEDA or provincial law.",
	},
	{
		place: "Australia",
		consent: "mixed",
		note: "By state. Victoria and Queensland let a participant record; New South Wales, Western Australia, South Australia, Tasmania and the ACT need consent, with narrow exceptions.",
	},
	{
		place: "Japan, Brazil, India",
		consent: "one",
		note: "A participant may generally record. Work use falls under APPI, LGPD and the DPDP Act.",
	},
];

export interface RuleSet {
	what: string;
	law: string;
	detail: string;
}

/** What Steno keeps, and which body of law cares about it. */
export const ruleSets: RuleSet[] = [
	{
		what: "The audio file",
		law: "Recording law",
		detail:
			"A stored file is what most statutes call a recording. Steno writes audio to disk before it transcribes, so this applies every time you press Record.",
	},
	{
		what: "Transcript, summary, tasks",
		law: "Data protection",
		detail:
			"For work, you are the controller under the GDPR and similar laws: you need a reason, people have to be told, and they can ask for a copy or for deletion. Purely private use is exempt in the EU.",
	},
	{
		what: "Voice profiles",
		law: "Biometric law",
		detail:
			"Steno remembers voices so it can name speakers in the next meeting. A voice profile that identifies a person is biometric data under GDPR Art. 9 and Illinois' BIPA, the strictest category: explicit, often written, consent.",
	},
	{
		what: "Text sent to your summary model",
		law: "Data protection and confidentiality",
		detail:
			"A cloud model is a third party. That matters for client confidentiality and legal privilege. A local model keeps the text on your machine.",
	},
];

export interface Situation {
	title: string;
	body: string;
}

export const situations: Situation[] = [
	{
		title: "Calls with friends and family",
		body: "Recording law still applies. Recording a friend in Germany or California without asking is unlawful even though nobody else ever hears it. Data protection law does not apply to purely private use in the EU.",
	},
	{
		title: "Your own work calls",
		body: "Freelancers, founders and team leads are the controller for their notes. Tell people what you record and why, delete when someone asks, and get explicit consent before Steno learns a person's voice.",
	},
	{
		title: "A company rolling Steno out",
		body: "Write a policy before the first install. In Germany a works council has a say in tools that can monitor staff (BetrVG § 87). In Illinois, companies that enable voice recognition have been named in BIPA suits next to the vendor.",
	},
	{
		title: "Lawyers, doctors, therapists",
		body: "Professional secrecy rules restrict sending client material to an outside provider, and a US court has held that material run through a consumer AI service is not privileged. Use a local summary model, or none.",
	},
];

export interface Case {
	name: string;
	when: string;
	held: string;
	forSteno: string;
	href: string;
}

export const cases: Case[] = [
	{
		name: "In re Otter.AI Privacy Litigation",
		when: "N.D. Cal., ruling of 13 Aug 2026",
		held: "Wiretap, California privacy and BIPA claims go ahead. Otter can be a third-party eavesdropper because it keeps and uses recordings for its own purposes.",
		forSteno:
			"There is no Steno server, so no vendor can be the eavesdropper. Your own duty to the people on the call is unchanged.",
		href: "https://caselaw.findlaw.com/court/us-dis-crt-n-d-cal/322025.html",
	},
	{
		name: "Chamberlain v. Granola",
		when: "N.D. Cal., filed 30 Jul 2026",
		held: "The first suit against a notetaker without a bot. It argues that recording from the user's computer, invisible to everyone else, was a design choice to avoid disclosure. No ruling yet.",
		forSteno:
			"Steno captures audio the same way. Telling people yourself is what closes that gap.",
		href: "https://btlaw.com/en/insights/alerts/2026/what-the-granola-class-action-means-for-companies-building-and-deploying-conversation-capture-tools",
	},
	{
		name: "Cruz and Fricker v. Fireflies.AI",
		when: "Illinois, Dec 2025 and Mar 2026",
		held: "BIPA claims based on speaker recognition alone: voiceprints of people without an account, no written consent, no published retention policy.",
		forSteno:
			"Steno's voice profiles are voiceprints too, kept on your computer. BIPA binds companies, so a company deploying Steno carries this.",
		href: "https://www.ebglaw.com/insights/publications/ai-meeting-assistants-and-biometric-privacy-lessons-from-the-fireflies-ai-lawsuit",
	},
	{
		name: "United States v. Heppner",
		when: "S.D.N.Y., 17 Feb 2026",
		held: "Documents a defendant produced with a consumer AI service were not protected by attorney-client privilege.",
		forSteno:
			"Your summary model is the third party here. For privileged conversations, use a local model.",
		href: "https://www.proskauer.com/alert/sdny-addresses-privilege-and-work-product-implications-of-using-unsecured-public-ai-tools",
	},
	{
		name: "EU AI Act",
		when: "High-risk rules from 2 Dec 2027",
		held: "The 2026 Digital Omnibus moved the high-risk obligations, which include some biometric identification, to December 2027. Emotion recognition at work has been banned since February 2025.",
		forSteno:
			"Steno does no emotion recognition. Whether matching voices of people in a meeting counts as high-risk identification is disputed.",
		href: "https://www.gibsondunn.com/eu-ai-act-omnibus-agreement-postponed-high-risk-deadlines-and-other-key-changes/",
	},
];

export interface Gap {
	title: string;
	body: string;
}

/** What Steno does not do for you today. Remove a row when the app does it. */
export const gaps: Gap[] = [
	{
		title: "It doesn't tell anyone.",
		body: "No bot joins, nothing beeps, no notice reaches the other side. Saying it is your job.",
	},
	{
		title: "It keeps audio until you say otherwise.",
		body: "New installs keep recordings forever. Settings → Recording deletes them after processing or after a number of days, with a per-meeting override.",
	},
	{
		title: "Voice profiles outlive meetings.",
		body: "Deleting a meeting removes its recording, transcript, summary and tasks. The people Steno knows, their voice profiles and any files you already exported stay. There is no button yet to forget one person's voice.",
	},
	{
		title: "It doesn't record consent.",
		body: "Steno has no field for who agreed. Keep that in your notes or your calendar.",
	},
];

export const disclosure = {
	en: "Quick note before we start: I record this call on my computer to take notes. The recording stays on my machine. Tell me now if you'd rather I didn't.",
	de: "Kurz vorab: Ich zeichne das Gespräch auf meinem Rechner auf, um Notizen zu machen. Die Aufnahme bleibt auf meinem Gerät. Sag gern jetzt, wenn du das nicht möchtest.",
	invite:
		"I take notes with Steno, which records the call on my computer. The recording stays with me. Reply if you'd rather I didn't record.",
};

export interface Source {
	label: string;
	href: string;
}

export const sources: Source[] = [
	{
		label: "In re Otter.AI Privacy Litigation, order of 13 Aug 2026 (FindLaw)",
		href: "https://caselaw.findlaw.com/court/us-dis-crt-n-d-cal/322025.html",
	},
	{
		label: "Otter.ai as third-party eavesdropper (National Law Review)",
		href: "https://natlawreview.com/article/invited-participant-or-third-party-eavesdropper-court-holds-otterai-third-party",
	},
	{
		label: "The Granola class action (Barnes & Thornburg)",
		href: "https://btlaw.com/en/insights/alerts/2026/what-the-granola-class-action-means-for-companies-building-and-deploying-conversation-capture-tools",
	},
	{
		label: "BIPA suits against AI notetakers (Amundsen Davis)",
		href: "https://www.amundsendavislaw.com/labor-employment-law-update/employers-beware-uptick-in-bipa-lawsuits-targeting-ai-note-taking-software",
	},
	{
		label: "Lessons from the Fireflies.AI lawsuit (Epstein Becker Green)",
		href: "https://www.ebglaw.com/insights/publications/ai-meeting-assistants-and-biometric-privacy-lessons-from-the-fireflies-ai-lawsuit",
	},
	{
		label: "United States v. Heppner (Proskauer)",
		href: "https://www.proskauer.com/alert/sdny-addresses-privilege-and-work-product-implications-of-using-unsecured-public-ai-tools",
	},
	{
		label: "EU AI Act omnibus agreement (Gibson Dunn)",
		href: "https://www.gibsondunn.com/eu-ai-act-omnibus-agreement-postponed-high-risk-deadlines-and-other-key-changes/",
	},
	{
		label: "Speaker identification and data protection (Ailance)",
		href: "https://2b-advice.com/en/2026/04/17/transcription-and-speaker-identification-data-protection-voice-match/",
	},
	{
		label: "KI-gestützte Meeting-Transkription und § 201 StGB (Werning)",
		href: "https://www.werning.com/fileadmin/KI-gest%C3%BCtzte_Meeting-Transkription_und__%E2%80%AF201_StGB.pdf",
	},
	{
		label: "KI-Transkription und § 201 StGB (unternehmensstrafrecht.de)",
		href: "https://www.unternehmensstrafrecht.de/ki-transkription-und-%C2%A7-201-stgb/",
	},
	{
		label: "Call recording laws by US state, 2026",
		href: "https://www.getnextphone.com/blog/call-recording-laws-by-state",
	},
];
