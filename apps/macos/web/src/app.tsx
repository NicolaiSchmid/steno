import { useEffect, useState } from "react";
import { TooltipProvider } from "@/components/ui";
import { StoriesPage } from "@/stories/stories-page";
import { MainWindow } from "@/windows/main/main-window";

/**
 * Hash routes: `#/main` (the default when the hash is empty) and
 * `#/stories` (every component). Query flags: `dark`; for the main window
 * `menu` and `picker` open the actions menu and the speaker picker on mount,
 * and the mock bridge reads `scenario` and `tab` (`src/bridge/mock-transport.ts`).
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

function useDocumentScheme(dark: boolean) {
	useEffect(() => {
		document.documentElement.classList.toggle("dark", dark);
	}, [dark]);
}

export function App() {
	const route = useRoute();
	useDocumentScheme(route.params.has("dark"));

	let page: React.ReactNode;
	if (route.path === "/stories") {
		page = <StoriesPage />;
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
