import { type ReactNode, useState } from "react";

export interface ThemeScope {
	dark: boolean;
	/** The column element; popups portal here so they inherit its scheme. */
	container: HTMLElement;
}

export interface ThemePairProps {
	title: string;
	children: (scope: ThemeScope) => ReactNode;
}

const columnClass =
	"relative flex flex-wrap items-center gap-3 rounded-xl border border-border bg-background p-5 text-foreground";

/**
 * Renders the same story twice, light beside dark. Children mount only once
 * the column exists, so a popup that opens on first render (`defaultOpen`)
 * already has its portal container.
 */
export function ThemePair({ title, children }: ThemePairProps) {
	const [light, setLight] = useState<HTMLDivElement | null>(null);
	const [dark, setDark] = useState<HTMLDivElement | null>(null);
	return (
		<section className="flex flex-col gap-2">
			<h2 className="m-0 font-medium text-muted-foreground text-sm">{title}</h2>
			<div className="grid grid-cols-2 gap-3">
				<div className={columnClass} ref={setLight}>
					{light ? children({ dark: false, container: light }) : null}
				</div>
				<div className={`dark ${columnClass}`} ref={setDark}>
					{dark ? children({ dark: true, container: dark }) : null}
				</div>
			</div>
		</section>
	);
}
