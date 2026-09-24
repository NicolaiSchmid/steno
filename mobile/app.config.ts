import type { ExpoConfig } from "expo/config";

/**
 * Steno iOS recorder app config (Expo SDK 57).
 *
 * Copied from fifthset's `mobile/app.config.ts` and trimmed to a dumb
 * recorder: no auth, push, deep links, widgets or analytics. What remains is
 * the variant scheme, OTA updates via the fingerprint policy, microphone and
 * background-audio capability, and the local-network keys the phone needs to
 * find the Mac over Bonjour. Runtime endpoints are NOT here — see
 * `src/lib/env.ts` and the `extra` comment below.
 */

const APP_VARIANT = (process.env.APP_VARIANT ?? "development") as
	| "development"
	| "preview"
	| "production";

// EAS workspace linkage. Minted by `eas init` (account @nicolaischmid); until
// then the id is empty and OTA updates are disabled below. Once minted, commit
// the uuid here as the default — it is public and non-secret.
const easProjectId = process.env.EAS_PROJECT_ID ?? "";
const expoOwner = process.env.EXPO_OWNER ?? "nicolaischmid";
// Apple Developer team (Nicolai Schmid, Individual) — the same team that
// owns fifthset and nunc-uebergabe.
const APPLE_TEAM_ID = "KQB68F43PW";

// IMMUTABLE identifiers — decided once. Bundle id, EAS projectId, ascAppId and
// slug can never change without losing testers, keychain-backed data and
// server-side version codes. `uno.schmid.steno` derives from schmid.uno;
// confirm before the first TestFlight build, not after.
export const VARIANTS = {
	development: {
		name: "Steno Dev",
		scheme: "steno-dev",
		bundleId: "uno.schmid.steno.dev",
	},
	preview: {
		name: "Steno Preview",
		scheme: "steno-preview",
		bundleId: "uno.schmid.steno.preview",
	},
	production: {
		name: "Steno",
		scheme: "steno",
		bundleId: "uno.schmid.steno",
	},
} as const;
const VARIANT = VARIANTS[APP_VARIANT];

// Splash colours = the theme's canvases (global.css: dark #000000, light #fafafa).
const SPLASH_LIGHT = "#fafafa";
const SPLASH_DARK = "#000000";

// One microphone purpose string; German first because the store listing's
// primary locale is de-DE (see `locales` below for the iOS override).
const MICROPHONE_PERMISSION =
	"Steno records meetings and calls through the microphone.";

const config: ExpoConfig = {
	name: VARIANT.name,
	// Slug resolves the Expo project; rename it here only after renaming it on
	// expo.dev, or the CLI offers to create a second project instead.
	slug: "steno",
	scheme: [VARIANT.scheme],
	// `version` is part of the fingerprint: bumping it orphans OTAs for every
	// installed build, so bump it only together with a native release. Keep it
	// equal to mobile/package.json.
	version: "0.1.0",
	orientation: "portrait",
	userInterfaceStyle: "automatic",
	platforms: ["ios"],
	owner: expoOwner,
	// OTA updates via EAS Update, fingerprint runtime policy: EAS hashes the
	// native layer and only delivers a bundle to a build whose fingerprint
	// matches. A mismatch is silent, not a crash. Disabled until `eas init`
	// mints the project id.
	...(easProjectId
		? {
				updates: {
					enabled: true,
					url: `https://u.expo.dev/${easProjectId}`,
					fallbackToCacheTimeout: 0,
					checkAutomatically: "ON_LOAD",
				},
			}
		: {}),
	runtimeVersion: { policy: "fingerprint" },
	locales: {
		de: {
			ios: {
				CFBundleDisplayName: VARIANT.name,
				NSMicrophoneUsageDescription:
					"Steno nimmt Meetings und Anrufe über das Mikrofon auf.",
				NSLocalNetworkUsageDescription:
					"Steno sucht deinen Mac im lokalen Netzwerk, um Aufnahmen zu übertragen.",
			},
		},
	},
	ios: {
		supportsTablet: false,
		bundleIdentifier: VARIANT.bundleId,
		appleTeamId: APPLE_TEAM_ID,
		infoPlist: {
			ITSAppUsesNonExemptEncryption: false,
			// Keep recording when the phone locks or the user switches apps.
			UIBackgroundModes: ["audio"],
			// Bonjour discovery of the Mac listener (scope Q38). Both keys are
			// required on iOS 14+ or the local-network prompt never appears and
			// discovery silently returns nothing.
			NSLocalNetworkUsageDescription:
				"Steno looks for your Mac on the local network to hand over recordings.",
			NSBonjourServices: ["_steno._tcp"],
		},
	},
	plugins: [
		// Silence expo-dev-launcher's ambiguous script dependencies warning.
		"./plugins/withStripLocalNetworkPhaseFix",
		// NOTE: no `expo-build-properties` / `useFrameworks: "dynamic"` here, and
		// it must stay that way: RN 0.85+ ships React-Core as a prebuilt
		// xcframework and dynamic linkage breaks `<React/RCTBridge.h>`.
		"expo-secure-store",
		[
			"expo-audio",
			{
				microphonePermission: MICROPHONE_PERMISSION,
			},
		],
		[
			"expo-splash-screen",
			{
				backgroundColor: SPLASH_LIGHT,
				dark: { backgroundColor: SPLASH_DARK },
			},
		],
	],
	// `extra` is hashed into the native fingerprint (the `expoConfig`
	// fingerprint source), so ONLY build-coupled values belong here. Runtime
	// endpoints are read straight from `process.env.EXPO_PUBLIC_*` in
	// `src/lib/env.ts`, which Metro inlines into the JS bundle. Do not add
	// EXPO_PUBLIC_* values here.
	extra: {
		appVariant: APP_VARIANT,
		...(easProjectId ? { eas: { projectId: easProjectId } } : {}),
	},
};

export default config;
