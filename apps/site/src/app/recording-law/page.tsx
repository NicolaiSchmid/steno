import type { Metadata } from "next";
import { alt, size } from "@/app/opengraph-image";
import { Footer } from "@/components/footer";
import { Header } from "@/components/header";
import { BeforeYouRecord } from "@/components/recording-law/before-you-record";
import { Cases } from "@/components/recording-law/cases";
import { ConsentTable } from "@/components/recording-law/consent-table";
import { Gaps } from "@/components/recording-law/gaps";
import { LawHero } from "@/components/recording-law/law-hero";
import { RuleSets } from "@/components/recording-law/rule-sets";
import { Situations } from "@/components/recording-law/situations";
import { Sources } from "@/components/recording-law/sources";
import { openGraphBase, twitterBase } from "@/lib/share";
import { site } from "@/lib/site";

const title = "Recording and the law";
// The layout's title template covers <title> only, not the share cards.
const shareTitle = `${title} · ${site.name}`;
const images = [{ url: "/opengraph-image", alt, ...size }];
const description =
	"Steno keeps meetings on your computer. Whether you may record them is a separate question: consent rules by country, data protection for work notes, voice profiles as biometric data, and what to say before you press Record.";

export const metadata: Metadata = {
	title,
	description,
	alternates: { canonical: site.recordingLaw },
	openGraph: {
		...openGraphBase,
		url: site.recordingLaw,
		title: shareTitle,
		description,
		images,
	},
	twitter: { ...twitterBase, title: shareTitle, description, images },
};

export default function RecordingLawPage() {
	return (
		<>
			<Header />
			<main className="flex flex-1 flex-col">
				<LawHero />
				<BeforeYouRecord />
				<ConsentTable />
				<RuleSets />
				<Situations />
				<Cases />
				<Gaps />
				<Sources />
			</main>
			<Footer />
		</>
	);
}
