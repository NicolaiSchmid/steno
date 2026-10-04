import type { Metadata } from "next";
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

const description =
	"Steno keeps meetings on your computer. Whether you may record them is a separate question: consent rules by country, data protection for work notes, voice profiles as biometric data, and what to say before you press Record.";

export const metadata: Metadata = {
	title: "Recording and the law",
	description,
	alternates: { canonical: "/recording-law" },
	openGraph: {
		url: "/recording-law",
		title: "Recording and the law · Steno",
		description,
	},
	twitter: { title: "Recording and the law · Steno", description },
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
