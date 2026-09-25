import { createNativeStackNavigator } from "@react-navigation/native-stack";
import * as SplashScreen from "expo-splash-screen";
import { useEffect } from "react";

import { PairingSheet } from "@/features/pairing/PairingSheet";
import { RecorderScreen } from "@/features/recorder/RecorderScreen";
import type { RootStackParamList } from "./types";

const Stack = createNativeStackNavigator<RootStackParamList>();

export function RootNavigator() {
	// The splash is held open in index.ts; release it once the first screen
	// is mounted and themed.
	useEffect(() => {
		void SplashScreen.hideAsync();
	}, []);

	return (
		<Stack.Navigator screenOptions={{ headerShown: false }}>
			<Stack.Screen component={RecorderScreen} name="Home" />
			<Stack.Screen
				component={PairingSheet}
				name="Pairing"
				options={{ presentation: "modal" }}
			/>
		</Stack.Navigator>
	);
}
