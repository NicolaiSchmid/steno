import { XIcon } from "lucide-react";
import { useRef } from "react";
import { Button, RecordMark } from "@/components/ui";
import { useCountdownSeconds } from "@/lib/use-now";
import { CountdownHairline, PanelBar } from "./panel-bar";
import {
	type PanelAction,
	type PanelShell,
	panelShell,
	useReportSize,
} from "./panel-shell";

/**
 * The detection prompt
 * (`#/panel/prompt?app=<name>&seconds=<n>&raised=<n>`): the Swift
 * `DetectionPromptView` in the pill language. "<App> opened the
 * microphone", one line under it, one primary Record button, an X, and the
 * draining hairline along the bottom. No number: nothing is at stake when
 * the prompt closes. The shell opens the panel with the request in the
 * route and hides it when the host clears the prompt. Both buttons tell
 * the shell which prompt they were: the X dismisses it, Record has the
 * host's detection controller record the call, attributed to the app the
 * prompt named, which `recording.start` could not carry.
 */

export interface PromptRequest {
	appName: string;
	seconds: number;
	/**
	 * The shell's number for this prompt (`raised`), sent back with the X
	 * so a click that lands while the next prompt loads dismisses only the
	 * prompt it was aimed at.
	 */
	raised?: number;
}

/** The request from the route's query; a missing name is "An app". */
export function parsePromptRequest(params: URLSearchParams): PromptRequest {
	const seconds = Number(params.get("seconds"));
	const raised = Number(params.get("raised"));
	return {
		appName: params.get("app")?.trim() || "An app",
		seconds: Number.isFinite(seconds) && seconds > 0 ? seconds : 60,
		...(Number.isSafeInteger(raised) && raised > 0 ? { raised } : {}),
	};
}

export function DetectionPrompt({
	request,
	shell = panelShell(),
}: {
	request: PromptRequest;
	shell?: PanelShell;
}) {
	const remaining = Math.max(0, useCountdownSeconds(request.seconds));
	const bar = useRef<HTMLDivElement>(null);
	useReportSize(bar, shell);
	const answer = (action: PanelAction) => {
		const params =
			request.raised === undefined ? undefined : { raised: request.raised };
		shell.call(action, params).catch((cause: unknown) => {
			console.error(`panel: ${action} failed`, cause);
		});
	};

	return (
		<PanelBar
			className="relative h-14 min-w-[360px] max-w-[480px] gap-4 pr-2 pl-4"
			data-testid="detection-prompt"
			ref={bar}
		>
			<div className="flex min-w-0 flex-col gap-0.5">
				<p
					className="truncate font-semibold text-foreground text-sm"
					data-testid="prompt-title"
				>
					{request.appName} opened the microphone
				</p>
				<p className="truncate text-muted-foreground text-xs">
					Record with Steno? Audio stays on this device.
				</p>
			</div>
			<Button
				aria-label="Record with Steno"
				className="ml-auto"
				data-testid="prompt-record"
				onClick={() => answer("recordFromPrompt")}
				size="md"
				variant="primary"
			>
				<RecordMark />
				Record
			</Button>
			<Button
				aria-label="Not now"
				data-testid="prompt-dismiss"
				onClick={() => answer("dismissPrompt")}
				size="icon-xs"
				title="Not now"
				variant="ghost-muted"
			>
				<XIcon />
			</Button>
			<CountdownHairline fractionRemaining={remaining / request.seconds} />
		</PanelBar>
	);
}
