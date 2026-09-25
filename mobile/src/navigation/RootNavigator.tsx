import { createNativeStackNavigator } from "@react-navigation/native-stack";
import * as SplashScreen from "expo-splash-screen";
import { useEffect } from "react";
import { Platform } from "react-native";

import { PairingSheet } from "@/features/pairing/PairingSheet";
import { isSupportedIOS } from "@/features/recorder/ios-version";
import { RecorderScreen } from "@/features/recorder/RecorderScreen";
import { UpdateIOSScreen } from "@/features/recorder/UpdateIOSScreen";
import type { RootStackParamList } from "./types";

const Stack = createNativeStackNavigator<RootStackParamList>();

/** Below iOS 18.6 the recorder is replaced by the update screen (plan decision 10). */
const supported = Platform.OS !== "ios" || isSupportedIOS(Platform.Version);

function HomeScreen() {
	if (!supported) return <UpdateIOSScreen version={String(Platform.Version)} />;
	return <RecorderScreen />;
}

export function RootNavigator() {
	// The splash is held open in index.ts; release it once the first screen
	// is mounted and themed.
	useEffect(() => {
		void SplashScreen.hideAsync();
	}, []);

	return (
		<Stack.Navigator screenOptions={{ headerShown: false }}>
			<Stack.Screen component={HomeScreen} name="Home" />
			<Stack.Screen
				component={PairingSheet}
				name="Pairing"
				options={{ presentation: "modal" }}
			/>
		</Stack.Navigator>
	);
}
