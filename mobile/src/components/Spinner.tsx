import { useEffect } from "react";
import type { ColorValue, StyleProp, ViewStyle } from "react-native";
import Animated, {
	cancelAnimation,
	Easing,
	ReduceMotion,
	useAnimatedStyle,
	useSharedValue,
	withRepeat,
	withTiming,
} from "react-native-reanimated";
import Svg, { Path } from "react-native-svg";

import { useThemeColor } from "@/lib/useThemeColor";

/** lucide `loader-circle` geometry. 24-unit viewBox, 2pt stroke, round caps. */
const ARC = "M21 12a9 9 0 1 1-6.219-8.56";

/**
 * Replacement for every `ActivityIndicator`: an open arc on a 900ms linear
 * rotation loop, in the muted ink so it is themed everywhere.
 */
export function Spinner({
	accessibilityLabel = "Loading…",
	color,
	size = 20,
	style,
}: {
	accessibilityLabel?: string;
	color?: ColorValue;
	size?: number;
	style?: StyleProp<ViewStyle>;
}) {
	const mutedColor = useThemeColor("--color-muted-foreground");
	const rotation = useSharedValue(0);

	useEffect(() => {
		// This rotation IS the loading cue, not decoration. Under Reduce Motion
		// the default would park the arc at 360° — pixel-identical to the resting
		// 0° — leaving a frozen spinner. Essential progress indicators are the
		// documented exception. BOTH layers must opt out.
		rotation.value = withRepeat(
			withTiming(360, {
				duration: 900,
				easing: Easing.linear,
				reduceMotion: ReduceMotion.Never,
			}),
			-1,
			false,
			undefined,
			ReduceMotion.Never,
		);
		return () => cancelAnimation(rotation);
	}, [rotation]);

	const spin = useAnimatedStyle(() => ({
		transform: [{ rotate: `${rotation.value}deg` }],
	}));

	return (
		<Animated.View
			accessibilityLabel={accessibilityLabel}
			accessibilityRole="progressbar"
			style={[{ height: size, width: size }, spin, style]}
		>
			<Svg fill="none" height={size} viewBox="0 0 24 24" width={size}>
				<Path
					d={ARC}
					stroke={color ?? mutedColor}
					strokeLinecap="round"
					strokeWidth={2}
				/>
			</Svg>
		</Animated.View>
	);
}
