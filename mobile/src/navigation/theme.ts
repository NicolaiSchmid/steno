import { DarkTheme, DefaultTheme, type Theme } from "@react-navigation/native";

import { useResolvedAppearance } from "@/features/appearance/AppearanceProvider";
import { useThemeColor } from "@/lib/useThemeColor";

/**
 * React Navigation theme from the global.css tokens, so native-stack headers
 * and card backgrounds match the canvas instead of RN's stock greys.
 */
export function useNavigationTheme(): Theme {
	const scheme = useResolvedAppearance();
	const background = useThemeColor("--color-background");
	const strong = useThemeColor("--color-strong");
	const border = useThemeColor("--color-border");
	const destructive = useThemeColor("--color-destructive");
	const base = scheme === "dark" ? DarkTheme : DefaultTheme;
	return {
		...base,
		colors: {
			...base.colors,
			background: background as string,
			card: background as string,
			text: strong as string,
			border: border as string,
			primary: strong as string,
			notification: destructive as string,
		},
	};
}
