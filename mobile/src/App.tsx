import "../global.css";

// Per-weight entry points: the package index requires all 18 Geist faces,
// which Metro would then ship as assets (~1.6 MB) for the four we use.
import { Geist_400Regular } from "@expo-google-fonts/geist/400Regular";
import { Geist_500Medium } from "@expo-google-fonts/geist/500Medium";
import { Geist_600SemiBold } from "@expo-google-fonts/geist/600SemiBold";
import { Geist_700Bold } from "@expo-google-fonts/geist/700Bold";
import { NavigationContainer } from "@react-navigation/native";
import { useFonts } from "expo-font";
import { StatusBar } from "expo-status-bar";
import { GestureHandlerRootView } from "react-native-gesture-handler";
import { SafeAreaProvider } from "react-native-safe-area-context";

import { StartupGate } from "@/components/StartupGate";
import {
	AppearanceProvider,
	useResolvedAppearance,
} from "@/features/appearance/AppearanceProvider";
import { PairingProvider } from "@/features/pairing/PairingProvider";
import { QueueProvider } from "@/features/queue/QueueProvider";
import { RootNavigator } from "@/navigation/RootNavigator";
import { useNavigationTheme } from "@/navigation/theme";

function ThemedNavigation() {
	const colorScheme = useResolvedAppearance();
	const theme = useNavigationTheme();
	return (
		<>
			<NavigationContainer theme={theme}>
				<RootNavigator />
			</NavigationContainer>
			<StatusBar style={colorScheme === "dark" ? "light" : "dark"} />
		</>
	);
}

/**
 * Provider order (fifthset's, minus auth, data, analytics and OTA controller):
 * gesture root → safe area → StartupGate (above theme so it can render when
 * theme breaks) → Appearance → Pairing and Queue (the recorder's two stores)
 * → React Navigation.
 */
export default function App() {
	const [fontsLoaded, fontError] = useFonts({
		Geist_400Regular,
		Geist_500Medium,
		Geist_600SemiBold,
		Geist_700Bold,
	});
	// Render on failure too: returning null forever on a font error would
	// strand the held splash over an app that can never mount.
	if (!fontsLoaded && !fontError) return null;

	return (
		<GestureHandlerRootView style={{ flex: 1 }}>
			<SafeAreaProvider>
				<StartupGate>
					<AppearanceProvider>
						<PairingProvider>
							<QueueProvider>
								<ThemedNavigation />
							</QueueProvider>
						</PairingProvider>
					</AppearanceProvider>
				</StartupGate>
			</SafeAreaProvider>
		</GestureHandlerRootView>
	);
}
