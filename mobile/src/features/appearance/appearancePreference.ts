export type AppearancePreference = "system" | "light" | "dark";

export function parseAppearancePreference(
	value: string | null,
): AppearancePreference {
	return value === "light" || value === "dark" ? value : "system";
}
