import { ByoModel } from "@/components/byo-model";
import { ClosingCta } from "@/components/closing-cta";
import { Footer } from "@/components/footer";
import { Header } from "@/components/header";
import { Hero } from "@/components/hero";
import { HowItWorks } from "@/components/how-it-works";
import { OpenSource } from "@/components/open-source";
import { Privacy } from "@/components/privacy";
import { site } from "@/lib/site";

const jsonLd = {
	"@context": "https://schema.org",
	"@type": "SoftwareApplication",
	name: site.name,
	description: site.description,
	url: site.url,
	applicationCategory: "BusinessApplication",
	operatingSystem: "macOS, Windows, Linux",
	offers: { "@type": "Offer", price: "0", priceCurrency: "USD" },
	license: "https://opensource.org/licenses/MIT",
	downloadUrl: site.download,
	softwareHelp: site.repo,
	author: {
		"@type": "Person",
		name: site.author.name,
		url: site.author.url,
	},
};

export default function Page() {
	return (
		<>
			<Header />
			<main className="flex flex-1 flex-col">
				<Hero />
				<HowItWorks />
				<ByoModel />
				<Privacy />
				<OpenSource />
				<ClosingCta />
			</main>
			<Footer />
			<script
				// biome-ignore lint/security/noDangerouslySetInnerHtml: static JSON-LD built from constants above
				dangerouslySetInnerHTML={{ __html: JSON.stringify(jsonLd) }}
				type="application/ld+json"
			/>
		</>
	);
}
