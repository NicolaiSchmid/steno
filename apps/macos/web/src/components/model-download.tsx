import { CircleAlertIcon } from "lucide-react";
import { Callout, Disclosure } from "@/components/ui";

/**
 * A download's share as a whole percentage, or `null` before the first
 * share arrives (the bar is indeterminate, the label "…").
 */
export function downloadPercent(fraction: number | undefined): number | null {
	return fraction !== undefined && fraction > 0
		? Math.round(fraction * 100)
		: null;
}

/**
 * Under a model row whose download did not finish: the warning, and the
 * reason behind Details when the host sent one. Settings' Transcription
 * section and onboarding's speech models row share it.
 */
export function DownloadFailure({
	failure,
	testId,
}: {
	failure: string | undefined;
	testId: string;
}) {
	return (
		<div className="flex flex-col gap-1.5">
			<Callout
				icon={<CircleAlertIcon aria-hidden="true" />}
				size="sm"
				title="The download did not finish."
				variant="warning"
			/>
			{failure ? <Disclosure data-testid={testId}>{failure}</Disclosure> : null}
		</div>
	);
}
