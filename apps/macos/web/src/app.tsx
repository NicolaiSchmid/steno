import { useEffect, useState } from "react";
import { TooltipProvider } from "@/components/ui";
import { AppShellPreview, type ShellTab } from "@/stories/app-shell-preview";
import { StoriesPage } from "@/stories/stories-page";

/**
 * Hash routes for WP0: `#/shell` (the mockup rebuilt), `#/stories` (every
 * component). Query flags: `dark`, `tab=summary|transcript|tasks|notes`,
 * `menu`. The real windows replace this router in WP2.
 */

export interface Route {
	path: string;
	params: URLSearchParams;
}

export function parseHash(hash: string): Route {
	const raw = hash.startsWith("#") ? hash.slice(1) : hash;
	const [path = "", query = ""] = raw.split("?");
	return { path: path || "/shell", params: new URLSearchParams(query) };
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

function useDocumentScheme(dark: boolean) {
	useEffect(() => {
		document.documentElement.classList.toggle("dark", dark);
	}, [dark]);
}

const SHELL_TABS: readonly ShellTab[] = [
	"summary",
	"transcript",
	"tasks",
	"notes",
];

function shellTab(value: string | null): ShellTab {
	return SHELL_TABS.find((tab) => tab === value) ?? "summary";
}

export function App() {
	const route = useRoute();
	useDocumentScheme(route.params.has("dark"));

	let page: React.ReactNode;
	if (route.path === "/stories") {
		page = <StoriesPage />;
	} else {
		page = (
			<AppShellPreview
				key={`${route.params.get("tab")}-${route.params.has("menu")}`}
				menuOpen={route.params.has("menu")}
				tab={shellTab(route.params.get("tab"))}
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
