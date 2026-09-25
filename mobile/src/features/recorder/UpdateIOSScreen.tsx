import { Linking, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";

import { AppText } from "@/components/AppText";
import { PressableScale } from "@/components/PressableScale";
import { MINIMUM_IOS_VERSION } from "./ios-version";

/**
 * Shown instead of the recorder below iOS 18.6 (plan decision 10): older
 * iOS 18 builds lose the Local Network privilege state, so pairing and
 * handover would fail silently.
 */
export function UpdateIOSScreen({ version }: { version: string }) {
	return (
		<SafeAreaView className="flex-1 bg-background">
			<View className="flex-1 justify-center gap-4 px-6">
				<AppText variant="title">Update iOS to use Steno</AppText>
				<AppText variant="body">
					Steno needs iOS {MINIMUM_IOS_VERSION} or later to find your Mac on the
					local network reliably. This iPhone runs iOS {version}.
				</AppText>
				<PressableScale
					accessibilityRole="button"
					onPress={() =>
						void Linking.openURL(
							"App-Prefs:root=General&path=SOFTWARE_UPDATE_LINK",
						)
					}
				>
					<View className="self-start rounded-2xl bg-primary px-5 py-3">
						<AppText className="text-primary-foreground" variant="heading">
							Open Software Update
						</AppText>
					</View>
				</PressableScale>
			</View>
		</SafeAreaView>
	);
}
