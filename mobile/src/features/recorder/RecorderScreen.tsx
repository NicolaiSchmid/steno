import { View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";

import { AppText } from "@/components/AppText";

/**
 * Placeholder for the one screen the scope gives the phone: record, stop,
 * list with sync status. Recording (expo-audio), the local queue and the
 * Bonjour handover to the Mac land here in their own plans.
 */
export function RecorderScreen() {
	return (
		<SafeAreaView className="flex-1 bg-background">
			<View className="flex-1 gap-2 px-6 pt-8">
				<AppText variant="title">Steno</AppText>
				<AppText variant="muted">Recorder scaffold. No recording yet.</AppText>
			</View>
		</SafeAreaView>
	);
}
