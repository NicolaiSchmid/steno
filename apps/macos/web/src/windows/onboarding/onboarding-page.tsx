import type { ReactNode } from "react";
import { FooterBand, ScrollArea } from "@/components/ui";
import { cn } from "@/lib/cn";
import { usePlatform } from "@/lib/platform";

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
 * page's only heading, the intro, then the cards, scrolling under a pinned
 * footer band with the page's buttons (the dialog footer). On the Mac the 52 px top inset leaves the traffic lights their
 * room; elsewhere the native title bar sits above the page and the top is
 * the sides' 24 px. Either keeps the step caption clear of the 1.5 rem
 * scroll fade.
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
						"flex flex-col gap-6 px-6 pb-6",
						titleBarInset ? "pt-13" : "pt-6",
					)}
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
