import { XIcon } from "lucide-react";
import { useRef } from "react";
import appIcon from "@/assets/app-icon.png";
import { Button } from "@/components/ui";
import { cn } from "@/lib/cn";
import { PanelBar, softButtonClass } from "./panel-bar";
import {
	type PanelAction,
	type PanelShell,
	panelShell,
	useReportSize,
} from "./panel-shell";

/**
 * The detection prompt
 * (`#/panel/prompt?app=<name>&seconds=<n>&raised=<n>`), laid out as
 * Jamie's: "<App> opened the microphone" and one line under it, a soft
 * Record button carrying the app icon, and a quiet X. No countdown on
 * screen: nothing is at stake when the prompt closes, and the host times
 * it out. The shell opens the panel with the request in the
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
			className="h-18 min-w-[400px] max-w-[560px] gap-5 pr-3 pl-6"
			data-testid="detection-prompt"
			ref={bar}
		>
			<div className="flex min-w-0 flex-col gap-0.5">
				<p
					className="truncate font-semibold text-foreground text-reading"
					data-testid="prompt-title"
				>
					{request.appName} opened the microphone
				</p>
				<p className="truncate text-label text-muted-foreground">
					Record with Steno? Audio stays on this device.
				</p>
			</div>
			<button
				aria-label="Record with Steno"
				className={cn(
					softButtonClass,
					"ml-auto h-12 gap-2.5 pr-4 pl-2.5 font-semibold text-reading",
				)}
				data-testid="prompt-record"
				onClick={() => answer("recordFromPrompt")}
				type="button"
			>
				<img
					alt=""
					className="size-7 rounded-md"
					draggable={false}
					height={28}
					src={appIcon}
					width={28}
				/>
				Record
			</button>
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
		</PanelBar>
	);
}
