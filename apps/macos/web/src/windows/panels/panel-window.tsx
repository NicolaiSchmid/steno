import { useEffect } from "react";
import { DetectionPrompt, parsePromptRequest } from "./detection-prompt";
import { RecordingBubble } from "./recording-bubble";

/**
 * The floating panels' routes, served to the Tauri shell's two panel
 * webviews (`apps/desktop/src-tauri/src/panels.rs`): `#/panel/bubble` and
 * `#/panel/prompt?app=<name>&seconds=<n>&raised=<serial>`. The Swift app
 * draws these two surfaces in SwiftUI and never loads the routes. The page
 * is the pill alone on a transparent canvas, pinned to the top-left
 * corner, so the window the shell sizes from the pill's report shows
 * nothing else. The shell keeps the prompt's window and navigates it to
 * each new request; the query is the prompt's key, so a new request (the
 * shell numbers each one it raises, hence `raised`) mounts a new prompt
 * whose countdown starts from the top. The wrapper is `max-content` wide:
 * the shell sizes the window to the last report, and a pill laid out
 * inside that viewport could never report a larger size (the bubble grows
 * when the recorder starts), so the pill is measured at its own width,
 * never the window's. The document itself never scrolls (`html.panel` in
 * `theme.css`): the window is the pill's size rounded up, and a fraction
 * of overflow would otherwise summon WebKitGTK's scrollbars.
 */

export const PANEL_ROUTES = ["/panel/bubble", "/panel/prompt"] as const;
export type PanelRoute = (typeof PANEL_ROUTES)[number];

export function isPanelRoute(path: string): path is PanelRoute {
	return (PANEL_ROUTES as readonly string[]).includes(path);
}

/** Marks the document as a panel's while `panel` holds. */
export function usePanelDocument(panel: boolean): void {
	useEffect(() => {
		document.documentElement.classList.toggle("panel", panel);
	}, [panel]);
}

export function PanelWindow({
	route,
	params,
}: {
	route: PanelRoute;
	params: URLSearchParams;
}) {
	return (
		<div className="inline-flex w-max" data-testid="panel-window">
			{route === "/panel/bubble" ? (
				<RecordingBubble />
			) : (
				<DetectionPrompt
					key={params.toString()}
					request={parsePromptRequest(params)}
				/>
			)}
		</div>
	);
}
