import { useEffect, useMemo, useState } from "react";
import { TooltipProvider } from "@/components/ui";
import { StoriesPage } from "@/stories/stories-page";
import { MainWindow } from "@/windows/main/main-window";
import { isSectionId } from "@/windows/settings/sections";
import { SettingsWindow } from "@/windows/settings/settings-window";

/**
 * Hash routes: `#/main` (the default when the hash is empty),
 * `#/settings?section=<general|recording|transcription|summaries|export|iphone>`
 * and `#/stories` (every component). Query flags: `dark`; for the main window
 * `menu` and `picker` open the actions menu and the speaker picker on mount,
 * and the mock bridge reads `scenario` and `tab` (`src/bridge/mock-transport.ts`).
 * A hash change re-renders the page in place; nothing reloads.
 */

export interface Route {
	path: string;
	params: URLSearchParams;
}

export function parseHash(hash: string): Route {
	const raw = hash.startsWith("#") ? hash.slice(1) : hash;
	const [path = "", query = ""] = raw.split("?");
	return { path: path || "/main", params: new URLSearchParams(query) };
}

function useRoute(): Route {
	const [route, setRoute] = useState(() => parseHash(window.location.hash));
	useEffect(() => {
		const onChange = () => setRoute(parseHash(window.location.hash));
		window.addEventListener("hashchange", onChange);
		return () => window.removeEventListener("hashchange", onChange);
	}, []);
	return route;
}

/**
 * Whether the host wants dark: `WKWebView` reports the app's appearance as
 * `prefers-color-scheme`, so the pages follow the Mac's (or the UI test's)
 * appearance without a bridge round trip. Playwright and jsdom have no
 * dark preference, so screens force it with the `dark` query flag.
 */
function usePrefersDark(): boolean {
	const query = useMemo(
		() =>
			typeof window.matchMedia === "function"
				? window.matchMedia("(prefers-color-scheme: dark)")
				: null,
		[],
	);
	const [dark, setDark] = useState(query?.matches ?? false);
	useEffect(() => {
		if (!query) return;
		const update = () => setDark(query.matches);
		query.addEventListener("change", update);
		return () => query.removeEventListener("change", update);
	}, [query]);
	return dark;
}

function useDocumentScheme(dark: boolean) {
	useEffect(() => {
		document.documentElement.classList.toggle("dark", dark);
	}, [dark]);
}

export function App() {
	const route = useRoute();
	const prefersDark = usePrefersDark();
	useDocumentScheme(route.params.has("dark") || prefersDark);

	let page: React.ReactNode;
	if (route.path === "/stories") {
		page = <StoriesPage />;
	} else if (route.path === "/settings") {
		const section = route.params.get("section");
		page = (
			<SettingsWindow section={isSectionId(section) ? section : undefined} />
		);
	} else {
		page = (
			<MainWindow
				key={`${route.params.has("menu")}-${route.params.has("picker")}`}
				menuOpen={route.params.has("menu")}
				pickerOpen={route.params.has("picker")}
			/>
		);
	}

	return (
		<TooltipProvider delay={400}>
			<div className="h-full bg-background" data-ready="true">
				{page}
			</div>
		</TooltipProvider>
	);
}
