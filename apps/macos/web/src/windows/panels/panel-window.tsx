import { DetectionPrompt, parsePromptRequest } from "./detection-prompt";
import { RecordingBubble } from "./recording-bubble";

/**
 * The floating panels' routes, served to the Tauri shell's two panel
 * webviews (`apps/desktop/src-tauri/src/panels.rs`): `#/panel/bubble` and
 * `#/panel/prompt?app=<name>&seconds=<n>`. The Swift app draws these two
 * surfaces in SwiftUI and never loads the routes. The page is the pill
 * alone on a transparent canvas, pinned to the top-left corner, so the
 * window the shell sizes from the pill's report shows nothing else.
 */

export const PANEL_ROUTES = ["/panel/bubble", "/panel/prompt"] as const;
export type PanelRoute = (typeof PANEL_ROUTES)[number];

export function isPanelRoute(path: string): path is PanelRoute {
	return (PANEL_ROUTES as readonly string[]).includes(path);
}

export function PanelWindow({
	route,
	params,
}: {
	route: PanelRoute;
	params: URLSearchParams;
}) {
	return (
		<div className="inline-flex" data-testid="panel-window">
			{route === "/panel/bubble" ? (
				<RecordingBubble />
			) : (
				<DetectionPrompt request={parsePromptRequest(params)} />
			)}
		</div>
	);
}
