import type { ReactNode } from "react";
import { ScrollArea } from "@/components/ui";

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
 * scrolling under a pinned footer with the page's buttons. The 52 pt top
 * inset leaves the traffic lights their room.
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
			<ScrollArea className="min-h-0 flex-1">
				<div
					className="flex flex-col gap-5 px-8 pt-[52px] pb-5"
					data-testid={testId}
				>
					<header className="flex flex-col gap-1.5">
						<p
							className="m-0 text-[12px] text-faint"
							data-testid="onboarding-step"
						>
							Step {step} of 2
						</p>
						<h1
							className="m-0 font-semibold text-[26px] leading-[1.15] tracking-[-0.02em]"
							data-testid="onboarding-title"
						>
							{title}
						</h1>
						<p
							className="m-0 text-[14px] text-muted-foreground leading-[1.45]"
							data-testid="onboarding-intro"
						>
							{intro}
						</p>
						{aside ? (
							<p
								className="m-0 text-[12px] text-faint leading-[1.45]"
								data-testid="onboarding-retention"
							>
								{aside}
							</p>
						) : null}
					</header>
					{children}
				</div>
			</ScrollArea>
			<footer className="flex shrink-0 items-center justify-end gap-2 border-border border-t px-8 py-4">
				{footer}
			</footer>
		</>
	);
}
