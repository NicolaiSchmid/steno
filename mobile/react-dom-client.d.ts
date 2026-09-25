// `react-dom` ships without types and `@types/react-dom` is not a dependency
// of the app; the hook tests mount through `react-dom/client` under
// happy-dom and need only these two members.
declare module "react-dom/client" {
	import type { ReactNode } from "react";

	export type Root = {
		render(children: ReactNode): void;
		unmount(): void;
	};
	export function createRoot(container: Element | DocumentFragment): Root;
}
