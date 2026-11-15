import type { ReactNode } from "react";
import { FooterBand, ScrollArea } from "@/components/ui";
import { cn } from "@/lib/cn";
import { usePlatform } from "@/lib/platform";

export interface OnboardingPageProps {
	/** The page's place in the two steps; the import step before them has none. */
	step?: 1 | 2 | undefined;
	title: string;
	/** The one sentence under the title. */
	intro: string;
	/** A quieter line under the rows (what the retention rule does). */
	aside?: string | undefined;
	/** The page's buttons, trailing. */
	footer: ReactNode;
	testId: string;
	children?: ReactNode;
}

/**
 * The frame the onboarding pages share: the title as the page's only
 * heading, the intro, then the rows and the quieter aside under them,
 * scrolling under a pinned footer band with the step caption at its leading
 * edge and the page's buttons at its trailing one (the dialog footer). The
 * page fits its 560 by 620 window with every permission row showing. On the Mac the
 * 52 px top inset leaves the traffic lights their room; elsewhere the
 * native title bar sits above the page and the top is the sides' 24 px.
 * Either keeps the title clear of the 1.5 rem scroll fade.
 */
export function OnboardingPage({
	step,
	title,
	intro,
	aside,
	footer,
	testId,
	children,
}: OnboardingPageProps) {
	const { titleBarInset } = usePlatform();
	return (
		<>
			<ScrollArea className="min-h-0 flex-1" fade>
				<div
					className={cn(
						"flex flex-col gap-4 px-6 pb-6",
						titleBarInset ? "pt-13" : "pt-6",
					)}
					data-testid={testId}
				>
					<header className="flex flex-col gap-1">
						<h1 className="m-0 text-display" data-testid="onboarding-title">
							{title}
						</h1>
						<p
							className="m-0 text-body text-muted-foreground"
							data-testid="onboarding-intro"
						>
							{intro}
						</p>
					</header>
					{children}
					{aside ? (
						<p
							className="m-0 text-meta text-muted-foreground"
							data-testid="onboarding-retention"
						>
							{aside}
						</p>
					) : null}
				</div>
			</ScrollArea>
			<FooterBand className="shrink-0">
				{step ? (
					<p
						className="m-0 mr-auto text-meta text-muted-foreground"
						data-testid="onboarding-step"
					>
						Step {step} of 2
					</p>
				) : null}
				{footer}
			</FooterBand>
		</>
	);
}
