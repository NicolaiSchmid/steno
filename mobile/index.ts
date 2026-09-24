import "react-native-gesture-handler";
import { registerRootComponent } from "expo";
import * as SplashScreen from "expo-splash-screen";

import App from "./src/App";

// Keep the native splash up through font loading and the auth restore.
// Without this it auto-hides the moment the root view attaches — before fonts
// resolve — flashing a bare frame and then an un-themed spinner. RootNavigator
// hides it once auth settles (capped so a wedged restore still reveals the
// themed gate), and StartupGate's diagnostic screen hides it when startup
// fails above that.
SplashScreen.setOptions({ duration: 250, fade: true });
void SplashScreen.preventAutoHideAsync();

// Last-resort backstop: holding the splash means the app can no longer fail
// *visibly*. If no React tree ever commits (a throw above every boundary, a
// wedged font fetch), this guarantees the user still reaches whatever did
// render instead of staring at a frozen launch image.
setTimeout(() => void SplashScreen.hideAsync(), 6_000);

registerRootComponent(App);
