import type { ColorValue } from "react-native";
import { useCSSVariable } from "uniwind";

/**
 * Typed wrapper around uniwind's `useCSSVariable` that returns a `ColorValue`
 * for use in React Native style props (backgroundColor, borderColor, tintColor).
 * Reads the `--color-*` tokens defined in global.css (light + dark). Verbatim
 * from t3code so its components read theme colors the same way.
 *
 * Usage: `const color = useThemeColor("--color-separator");`
 */
export function useThemeColor(variable: `--color-${string}`): ColorValue {
	return useCSSVariable(variable) as string as ColorValue;
}
