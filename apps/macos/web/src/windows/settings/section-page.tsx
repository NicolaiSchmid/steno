import { CircleAlertIcon } from "lucide-react";
import type { ReactNode } from "react";
import { Callout, Disclosure } from "@/components/ui";
import { usePlatform } from "@/lib/platform";
import { type SectionId, sectionInfo } from "./sections";

export interface SectionPageProps {
	id: SectionId;
	/** The section's error sentence and the original text behind it. */
	error?: string | undefined;
	errorDetails?: string | undefined;
	children?: ReactNode;
}

/**
 * The shape every section shares: the page title and its one sentence of
 * purpose, the section's error (if any) with its details folded away, then
 * the flat groups at the settings rhythm.
 */
export function SectionPage({
	id,
	error,
	errorDetails,
	children,
}: SectionPageProps) {
	const info = sectionInfo(id);
	const { words } = usePlatform();
	return (
		<div className="flex flex-col gap-8" data-testid={`section-${id}`}>
			<header className="flex flex-col gap-1">
				<h1 className="m-0 text-display" data-testid={`section-title-${id}`}>
					{info.title}
				</h1>
				<p
					className="m-0 text-body text-muted-foreground"
					data-testid={`section-purpose-${id}`}
				>
					{info.purpose(words)}
				</p>
			</header>
			{error ? (
				<div className="flex flex-col gap-1.5" data-testid="section-error">
					<Callout
						icon={<CircleAlertIcon aria-hidden="true" />}
						title={error}
						variant="destructive"
					/>
					{errorDetails ? (
						<Disclosure data-testid="section-error-details">
							{errorDetails}
						</Disclosure>
					) : null}
				</div>
			) : null}
			{children}
		</div>
	);
}
