import type { ReactNode } from "react";
import { FooterBand, ScrollArea } from "@/components/ui";

export interface OnboardingPageProps {
	step: 1 | 2;
	title: string;
	/** The one sentence under the title. */
	intro: string;
	/** A quieter line under the intro (what the retention rule does). */
	aside?: string | undefined;
	/** The page's buttons, trailing. */
	footer: ReactNode;
	testId: string;
	children?: ReactNode;
}

/**
 * The frame both onboarding pages share: the step caption, the title as the
 * window's only heading (there is no title bar), the intro, then the cards,
 * scrolling under a pinned footer band with the page's buttons (the dialog
 * footer). The 52 px top inset leaves the traffic lights their room and
 * keeps the step caption clear of the scroll fade.
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
	return (
		<>
			<ScrollArea className="min-h-0 flex-1" fade>
				<div
					className="flex flex-col gap-6 px-6 pt-13 pb-6"
					data-testid={testId}
				>
					<header className="flex flex-col gap-2">
						<p
							className="m-0 text-muted-foreground text-xs"
							data-testid="onboarding-step"
						>
							Step {step} of 2
						</p>
						<h1
							className="m-0 font-semibold text-2xl leading-tight tracking-tight"
							data-testid="onboarding-title"
						>
							{title}
						</h1>
						<p
							className="m-0 text-muted-foreground text-sm"
							data-testid="onboarding-intro"
						>
							{intro}
						</p>
						{aside ? (
							<p
								className="m-0 text-faint text-xs"
								data-testid="onboarding-retention"
							>
								{aside}
							</p>
						) : null}
					</header>
					{children}
				</div>
			</ScrollArea>
			<FooterBand className="shrink-0">{footer}</FooterBand>
		</>
	);
}
