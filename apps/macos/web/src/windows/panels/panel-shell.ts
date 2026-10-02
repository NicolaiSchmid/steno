import { invoke } from "@tauri-apps/api/core";
import { type RefObject, useEffect } from "react";
import { hasTauriBridge } from "@/bridge/tauri-transport";

/**
 * The panels' own line to the Tauri shell, beside the bridge: the
 * `panel_call` command (`apps/desktop/src-tauri/src/bridge.rs`). The
 * bridge carries the recorder; this carries what only a floating webview
 * needs, the measured size the shell sizes the window from and the
 * prompt's dismissal. Outside the shell (the dev server, the stories, the
 * tests) every call is a no-op, so the routes render anywhere.
 */

export const PANEL_CALL_COMMAND = "panel_call";

export type PanelAction = "resize" | "dismissPrompt";

export interface PanelShell {
	call(action: PanelAction, params?: unknown): Promise<void>;
}

export function createPanelShell(target: Window = window): PanelShell {
	if (!hasTauriBridge(target)) {
		return { call: async () => {} };
	}
	return {
		async call(action, params) {
			await invoke(PANEL_CALL_COMMAND, { action, params: params ?? null });
		},
	};
}

let shared: PanelShell | undefined;

/** The page's one shell handle; created on first use. */
export function panelShell(): PanelShell {
	shared ??= createPanelShell();
	return shared;
}

/**
 * Reports the element's size to the shell whenever it changes, so the
 * window takes the pill's intrinsic size (the Swift root's
 * `onGeometryChange`). The border box, in CSS pixels, which are the
 * shell's logical points.
 */
export function useReportSize(
	ref: RefObject<HTMLElement | null>,
	shell: PanelShell = panelShell(),
): void {
	useEffect(() => {
		const element = ref.current;
		if (!element || typeof ResizeObserver === "undefined") {
			return;
		}
		const report = () => {
			const { width, height } = element.getBoundingClientRect();
			if (width > 0 && height > 0) {
				shell.call("resize", { width, height }).catch((cause: unknown) => {
					console.error("panel: resize failed", cause);
				});
			}
		};
		report();
		const observer = new ResizeObserver(report);
		observer.observe(element);
		return () => observer.disconnect();
	}, [ref, shell]);
}
