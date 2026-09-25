import { View } from "react-native";
import Animated, {
	interpolate,
	useAnimatedStyle,
	useDerivedValue,
	withSpring,
	withTiming,
} from "react-native-reanimated";

import { PressableScale } from "@/components/PressableScale";
import { Spinner } from "@/components/Spinner";
import {
	DURATION_FUNCTIONAL,
	EASE_STANDARD,
	SPRING_SPATIAL,
} from "@/lib/motion";
import { useThemeColor } from "@/lib/useThemeColor";

/**
 * The one control: a ring with a red disc that morphs into a rounded square
 * while recording (the spatial spring), fading the ring to the emphasis tier
 * (functional tempo). `busy` shows a spinner while the session starts or the
 * file is being finalised.
 */
const SIZE = 88;
const INNER_IDLE = 68;
const INNER_RECORDING = 32;

export function RecordButton({
	recording,
	busy,
	onPress,
}: {
	recording: boolean;
	busy: boolean;
	onPress: () => void;
}) {
	const ring = useThemeColor("--color-border");
	const ringActive = useThemeColor("--color-ring");
	const disc = useThemeColor("--color-destructive");
	const progress = useDerivedValue(() =>
		withSpring(recording ? 1 : 0, SPRING_SPATIAL),
	);
	const ringProgress = useDerivedValue(() =>
		withTiming(recording ? 1 : 0, {
			duration: DURATION_FUNCTIONAL,
			easing: EASE_STANDARD,
		}),
	);

	const innerStyle = useAnimatedStyle(() => {
		const size = interpolate(
			progress.value,
			[0, 1],
			[INNER_IDLE, INNER_RECORDING],
		);
		return {
			width: size,
			height: size,
			borderRadius: interpolate(progress.value, [0, 1], [INNER_IDLE / 2, 8]),
			backgroundColor: disc as string,
		};
	});
	const ringStyle = useAnimatedStyle(() => ({
		borderColor:
			ringProgress.value > 0.5 ? (ringActive as string) : (ring as string),
	}));

	return (
		<PressableScale
			accessibilityHint={
				recording
					? "Stops and queues the recording for your Mac"
					: "Starts a new recording"
			}
			accessibilityLabel={recording ? "Stop recording" : "Start recording"}
			accessibilityRole="button"
			accessibilityState={{ busy, disabled: busy }}
			disabled={busy}
			onPress={onPress}
		>
			<Animated.View
				style={[
					{
						width: SIZE,
						height: SIZE,
						borderRadius: SIZE / 2,
						borderWidth: 2,
						alignItems: "center",
						justifyContent: "center",
					},
					ringStyle,
				]}
			>
				{busy ? (
					<View className="items-center justify-center">
						<Spinner accessibilityLabel="Working" size={28} />
					</View>
				) : (
					<Animated.View style={innerStyle} />
				)}
			</Animated.View>
		</PressableScale>
	);
}
