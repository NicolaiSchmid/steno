import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

/**
 * Joins class values and resolves Tailwind conflicts (the last class wins).
 * Used inside `components/ui`; callers pass layout classes only.
 */
export function cn(...inputs: ClassValue[]): string {
	return twMerge(clsx(inputs));
}
