import { CircleAlertIcon } from "lucide-react";
import type { ReactNode } from "react";
import { Callout, Disclosure } from "@/components/ui";
import { type SectionId, sectionInfo } from "./sections";

export interface SectionPageProps {
	id: SectionId;
	/** The section's error sentence and the original text behind it. */
	error?: string | undefined;
	errorDetails?: string | undefined;
	children?: ReactNode;
}

/**
 * The shape every section shares: the one sentence of purpose (the
 * breadcrumb in the header row carries the title), the section's error (if
 * any) with its details folded away, then the form cards at the settings
 * rhythm.
 */
export function SectionPage({
	id,
	error,
	errorDetails,
	children,
}: SectionPageProps) {
	const info = sectionInfo(id);
	return (
		<div
			className="flex flex-col gap-8 px-6 pt-4 pb-12"
			data-testid={`section-${id}`}
		>
			<p
				className="m-0 text-muted-foreground text-sm"
				data-testid={`section-title-${id}`}
			>
				{info.purpose}
			</p>
			{error ? (
				<div className="flex flex-col gap-1.5" data-testid="section-error">
					<Callout
						icon={<CircleAlertIcon aria-hidden="true" />}
						title={error}
						variant="warning"
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
