import type { PressableProps, StyleProp, ViewStyle } from "react-native";
import { Pressable } from "react-native";
import Animated, {
	interpolate,
	useAnimatedStyle,
	useSharedValue,
	withTiming,
} from "react-native-reanimated";

import {
	DURATION_FUNCTIONAL,
	DURATION_PRESS_IN,
	EASE_STANDARD,
	PRESS_OPACITY,
	PRESS_SCALE,
} from "@/lib/motion";

const AnimatedPressable = Animated.createAnimatedComponent(Pressable);

/**
 * The app's one press response: scale to 0.97 + dim to 0.85, in fast, out at
 * the functional tempo. Drop-in for `Pressable` — replaces both the instant
 * `opacity: pressed ? x : 1` steps and the bare no-feedback Pressables.
 *
 * `baseOpacity` carries a call site's resting/disabled opacity (the animated
 * style owns the `opacity` prop, so putting it in `style` would be clobbered).
 */
export function PressableScale({
	baseOpacity = 1,
	opacityTo = PRESS_OPACITY,
	scaleTo = PRESS_SCALE,
	style,
	...props
}: Omit<PressableProps, "style"> & {
	baseOpacity?: number;
	/** Pressed opacity target — full-width rows dim further instead of scaling. */
	opacityTo?: number;
	/** Pressed scale target — pass 1 to disable the scale for large surfaces. */
	scaleTo?: number;
	style?: StyleProp<ViewStyle>;
}) {
	const pressed = useSharedValue(0);

	const pressStyle = useAnimatedStyle(() => ({
		opacity: baseOpacity * interpolate(pressed.value, [0, 1], [1, opacityTo]),
		transform: [{ scale: interpolate(pressed.value, [0, 1], [1, scaleTo]) }],
	}));

	return (
		<AnimatedPressable
			{...props}
			onPressIn={(event) => {
				pressed.value = withTiming(1, {
					duration: DURATION_PRESS_IN,
					easing: EASE_STANDARD,
				});
				props.onPressIn?.(event);
			}}
			onPressOut={(event) => {
				pressed.value = withTiming(0, {
					duration: DURATION_FUNCTIONAL,
					easing: EASE_STANDARD,
				});
				props.onPressOut?.(event);
			}}
			style={[style, pressStyle]}
		/>
	);
}
