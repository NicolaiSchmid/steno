import { XIcon } from "lucide-react";
import { useRef } from "react";
import { send, useBridge } from "@/bridge/hooks";
import { Button, RecordMark } from "@/components/ui";
import { useCountdownSeconds } from "@/lib/use-now";
import { CountdownHairline, PanelBar } from "./panel-bar";
import { type PanelShell, panelShell, useReportSize } from "./panel-shell";

/**
 * The detection prompt (`#/panel/prompt?app=<name>&seconds=<n>`): the Swift
 * `DetectionPromptView` in the pill language. "<App> opened the
 * microphone", one line under it, one primary Record button, an X, and the
 * draining hairline along the bottom. No number: nothing is at stake when
 * the prompt closes. The shell opens the panel with the request in the
 * route and closes it when the host clears the prompt; Record starts a call
 * recording through the bridge, the X tells the shell.
 */

export interface PromptRequest {
	appName: string;
	seconds: number;
}

/** The request from the route's query; a missing name is "An app". */
export function parsePromptRequest(params: URLSearchParams): PromptRequest {
	const seconds = Number(params.get("seconds"));
	return {
		appName: params.get("app")?.trim() || "An app",
		seconds: Number.isFinite(seconds) && seconds > 0 ? seconds : 60,
	};
}

export function DetectionPrompt({
	request,
	shell = panelShell(),
}: {
	request: PromptRequest;
	shell?: PanelShell;
}) {
	const client = useBridge();
	const remaining = Math.max(0, useCountdownSeconds(request.seconds));
	const bar = useRef<HTMLDivElement>(null);
	useReportSize(bar, shell);

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
				onClick={() => send(client, "recording.start", { mode: "call" })}
				size="md"
				variant="primary"
			>
				<RecordMark />
				Record
			</Button>
			<Button
				aria-label="Not now"
				data-testid="prompt-dismiss"
				onClick={() => {
					shell.call("dismissPrompt").catch((cause: unknown) => {
						console.error("panel: dismissPrompt failed", cause);
					});
				}}
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
