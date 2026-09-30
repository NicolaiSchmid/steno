import { Progress as BaseProgress } from "@base-ui/react/progress";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

export interface ProgressBarProps
	extends Omit<ComponentProps<typeof BaseProgress.Root>, "className"> {
	className?: string;
}

/**
 * A thin track with a primary fill. Indeterminate (`value={null}`) shows a
 * third of the track pulsing with the stepped `animate-pulse` keyframes, so
 * nothing repaints continuously.
 */
export function ProgressBar({ className, ...props }: ProgressBarProps) {
	return (
		<BaseProgress.Root className={cn("block w-full", className)} {...props}>
			<BaseProgress.Track className="block h-1 w-full overflow-hidden rounded-full bg-accent">
				<BaseProgress.Indicator className="block h-full rounded-full bg-primary transition-[width] duration-(--duration-entrance) ease-standard data-indeterminate:w-1/3 data-indeterminate:animate-pulse" />
			</BaseProgress.Track>
		</BaseProgress.Root>
	);
}
