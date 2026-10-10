import { invoke } from "@tauri-apps/api/core";
import { type RefObject, useEffect } from "react";
import { hasTauriBridge } from "@/bridge/tauri-transport";

/**
 * The panels' own line to the Tauri shell, beside the bridge: the
 * `panel_call` command (`apps/desktop/src-tauri/src/bridge.rs`). The
 * bridge carries the recorder; this carries what only a floating webview
 * needs, the measured size the shell sizes the window from and the
 * prompt's two answers, its X and its Record. Outside the shell (the dev server, the stories, the
 * tests) every call is a no-op, so the routes render anywhere.
 */

export const PANEL_CALL_COMMAND = "panel_call";

export type PanelAction = "resize" | "dismissPrompt" | "recordFromPrompt";

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
 * The size the shell is sent for a measured box: its CSS pixels times
 * `devicePixelRatio`, so device pixels, which the shell divides by the
 * window's own scale factor. WebKitGTK sets the ratio from the X
 * resolution (1.25 at 120 dpi) while the window's scale stays 1, so CSS
 * pixels alone would size the window a fifth too small and clip the pill;
 * where the two agree (macOS, Windows) the result is the same either way.
 * `undefined` for a box that has no size yet.
 */
export function deviceSize(
	box: { width: number; height: number },
	ratio: number,
): { width: number; height: number } | undefined {
	if (!(box.width > 0 && box.height > 0)) {
		return undefined;
	}
	const scale = Number.isFinite(ratio) && ratio > 0 ? ratio : 1;
	return { width: box.width * scale, height: box.height * scale };
}

/**
 * Reports the element's size to the shell whenever it changes, so the
 * window takes the pill's intrinsic size (the Swift root's
 * `onGeometryChange`): the border box in device pixels (`deviceSize`).
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
			const size = deviceSize(
				element.getBoundingClientRect(),
				element.ownerDocument.defaultView?.devicePixelRatio ?? 1,
			);
			if (size) {
				shell.call("resize", size).catch((cause: unknown) => {
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
