import { type ClassValue, clsx } from "clsx";
import { extendTailwindMerge } from "tailwind-merge";

/**
 * The type scale in `src/theme.css`. tailwind-merge takes an unknown
 * `text-*` for a colour, so without this `text-meta text-muted-foreground`
 * would lose its size.
 */
const twMerge = extendTailwindMerge({
	extend: {
		theme: {
			text: [
				"display",
				"headline",
				"reading",
				"title",
				"body",
				"label",
				"meta",
				"caption",
			],
		},
	},
});

/**
 * Joins class values and resolves Tailwind conflicts (the last class wins).
 * Used inside `components/ui`; callers pass layout classes only.
 */
export function cn(...inputs: ClassValue[]): string {
	return twMerge(clsx(inputs));
}
