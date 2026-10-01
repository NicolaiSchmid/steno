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
 * Header plus cards, the shape every section shares: the title, the one
 * sentence of purpose, the section's error (if any) with its details folded
 * away, then the form cards.
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
			className="flex flex-col gap-4 px-7 pt-7 pb-8"
			data-testid={`section-${id}`}
		>
			<header className="flex flex-col gap-1">
				<h1
					className="m-0 font-semibold text-[16px] tracking-[-0.01em]"
					data-testid={`section-title-${id}`}
				>
					{info.title}
				</h1>
				<p className="m-0 text-[13px] text-muted-foreground leading-[1.45]">
					{info.purpose}
				</p>
			</header>
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
